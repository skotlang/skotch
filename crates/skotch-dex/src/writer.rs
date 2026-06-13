//! Byte-identical DEX writer. Builds the constant pools, sorts and indexes them
//! exactly as d8 does, lays out the data section in d8's canonical order, and
//! patches instruction operands and section offsets.

use crate::leb128::*;
use crate::model::*;
use crate::mutf8;
use std::collections::BTreeMap;

const HEADER_SIZE: usize = 0x70;
const ENDIAN_TAG: u32 = 0x1234_5678;

/// Writes a [`DexFile`] model to byte-identical-to-d8 `.dex` bytes.
pub fn write(file: &DexFile) -> Vec<u8> {
    let pools = Pools::build(file);
    pools.emit(file)
}

// ── Pools: collection, sorting, index assignment ─────────────────────────────

struct Pools {
    strings: Vec<String>,
    string_idx: BTreeMap<String, u32>,
    types: Vec<String>,
    type_idx: BTreeMap<String, u32>,
    protos: Vec<ProtoRef>,
    proto_idx: BTreeMap<ProtoRef, u32>,
    fields: Vec<FieldRef>,
    field_idx: BTreeMap<FieldRef, u32>,
    methods: Vec<MethodRef>,
    method_idx: BTreeMap<MethodRef, u32>,
    /// Distinct type lists (interfaces + proto params), as type-descriptor seqs.
    type_lists: Vec<Vec<String>>,
}

impl Pools {
    fn build(file: &DexFile) -> Pools {
        let mut strings: std::collections::BTreeSet<String> = Default::default();
        let mut types: std::collections::BTreeSet<String> = Default::default();
        let mut protos: std::collections::BTreeSet<ProtoRef> = Default::default();
        let mut fields: std::collections::BTreeSet<FieldRef> = Default::default();
        let mut methods: std::collections::BTreeSet<MethodRef> = Default::default();

        let add_type = |types: &mut std::collections::BTreeSet<String>,
                            strings: &mut std::collections::BTreeSet<String>,
                            t: &str| {
            types.insert(t.to_string());
            strings.insert(t.to_string());
        };

        for s in &file.extra_strings {
            strings.insert(s.clone());
        }

        for c in &file.classes {
            add_type(&mut types, &mut strings, &c.class_type);
            if let Some(s) = &c.superclass {
                add_type(&mut types, &mut strings, s);
            }
            for i in &c.interfaces {
                add_type(&mut types, &mut strings, i);
            }
            if let Some(sf) = &c.source_file {
                strings.insert(sf.clone());
            }
            for ef in c.static_fields.iter().chain(&c.instance_fields) {
                collect_field(&ef.field, &mut strings, &mut types, &mut fields);
            }
            for em in c.direct_methods.iter().chain(&c.virtual_methods) {
                collect_method(&em.method, &mut strings, &mut types, &mut protos, &mut methods);
                if let Some(code) = &em.code {
                    for fx in &code.fixups {
                        collect_itemref(&fx.item, &mut strings, &mut types, &mut protos, &mut fields, &mut methods);
                    }
                    for t in &code.tries {
                        for h in &t.handlers {
                            add_type(&mut types, &mut strings, &h.exception_type);
                        }
                    }
                    if let Some(di) = &code.debug_info {
                        collect_debug(di, &mut strings, &mut types);
                    }
                }
            }
        }

        // Assign indices in DEX-mandated order.
        let mut strings: Vec<String> = strings.into_iter().collect();
        strings.sort_by(|a, b| mutf8::cmp_utf16(a, b));
        let string_idx = index_map(&strings);

        let mut types: Vec<String> = types.into_iter().collect();
        types.sort_by(|a, b| string_idx[a].cmp(&string_idx[b]));
        let type_idx = index_map(&types);

        let mut protos: Vec<ProtoRef> = protos.into_iter().collect();
        protos.sort_by(|a, b| cmp_proto(a, b, &type_idx));
        let proto_idx = index_map(&protos);

        let mut fields: Vec<FieldRef> = fields.into_iter().collect();
        fields.sort_by(|a, b| cmp_field(a, b, &type_idx, &string_idx));
        let field_idx = index_map(&fields);

        let mut methods: Vec<MethodRef> = methods.into_iter().collect();
        methods.sort_by(|a, b| cmp_method(a, b, &type_idx, &string_idx, &proto_idx));
        let method_idx = index_map(&methods);

        // Distinct type lists in d8's layout order. `getTypeLists()` returns
        // INSERTION order, so d8 emits each list at its first occurrence while
        // walking the (already-sorted) protos and then class interfaces — NOT
        // sorted by content. (ConvAll's `(I)F`/`(J)F`/`(D)F` protos expose this:
        // type_lists come out D,I,J,F by first use, not D,F,I,J by content.)
        let mut type_lists: Vec<Vec<String>> = Vec::new();
        let mut tl_seen: std::collections::HashSet<Vec<String>> = Default::default();
        for p in &protos {
            if !p.params.is_empty() && tl_seen.insert(p.params.clone()) {
                type_lists.push(p.params.clone());
            }
        }
        for c in &file.classes {
            if !c.interfaces.is_empty() && tl_seen.insert(c.interfaces.clone()) {
                type_lists.push(c.interfaces.clone());
            }
        }

        Pools {
            strings,
            string_idx,
            types,
            type_idx,
            protos,
            proto_idx,
            fields,
            field_idx,
            methods,
            method_idx,
            type_lists,
        }
    }

    fn resolve(&self, item: &ItemRef) -> u32 {
        match item {
            ItemRef::String(s) => self.string_idx[s],
            ItemRef::Type(t) => self.type_idx[t],
            ItemRef::Field(f) => self.field_idx[f],
            ItemRef::Method(m) => self.method_idx[m],
            ItemRef::Proto(p) => self.proto_idx[p],
            ItemRef::CallSite(_) => 0, // not yet supported
        }
    }

    fn emit(&self, file: &DexFile) -> Vec<u8> {
        // Fixed-section offsets.
        let mut off = HEADER_SIZE;
        let string_ids_off = off;
        off += 4 * self.strings.len();
        let type_ids_off = off;
        off += 4 * self.types.len();
        let proto_ids_off = off;
        off += 12 * self.protos.len();
        let field_ids_off = if self.fields.is_empty() { 0 } else { off };
        off += 8 * self.fields.len();
        let method_ids_off = off;
        off += 8 * self.methods.len();
        let class_defs_off = off;
        off += 32 * file.classes.len();
        let data_off = off;

        // ── Build the data section (d8 canonical order). ──
        let mut data = DataSection::new(data_off);

        // 1. code (align 4). d8 lays code out in method-index order across all
        //    classes; keying offsets by MethodRef lets us reorder freely.
        let mut code_offsets: Vec<u32> = Vec::new();
        let mut method_code_off: BTreeMap<MethodRef, u32> = BTreeMap::new();
        let mut debug_for_code: Vec<(MethodRef, DebugInfo)> = Vec::new();
        let mut coded_methods: Vec<(&EncodedMethod,)> = Vec::new();
        for c in &file.classes {
            for m in c.direct_methods.iter().chain(&c.virtual_methods) {
                if m.code.is_some() {
                    coded_methods.push((m,));
                }
            }
        }
        // d8 lays codes out via DefaultMixedSectionLayoutStrategy:
        // `Comparator.comparing(getKeyForDexCodeSorting)`, where the key is
        // `holder.toSourceString() + MethodSignature.toString()` and the
        // signature stringifies RETURN-TYPE-FIRST (`type name(params)`). At
        // min-API < S, `canUseCanonicalizedCodeObjects()` is false, so the
        // dedup-count primary key is absent and this string key is the whole
        // comparator. (The API≥S count primary key is deferred to Phase 2.)
        coded_methods.sort_by(|(a,), (b,)| code_sort_key(&a.method).cmp(&code_sort_key(&b.method)));
        for (m,) in &coded_methods {
            let code = m.code.as_ref().unwrap();
            let coff = data.align(4);
            method_code_off.insert(m.method.clone(), coff);
            if let Some(di) = &code.debug_info {
                debug_for_code.push((m.method.clone(), di.clone()));
            }
            self.write_code_item(&mut data, code, code.debug_info.is_some());
            code_offsets.push(coff);
        }

        // 2. debug_info (align 1) — written after codes; patch code's debug_info_off.
        //    d8 canonicalizes debug_info_item objects: methods with byte-identical
        //    debug info (e.g. two default `<init>`s) share one item. We dedup by
        //    encoded bytes; distinct items keep first-occurrence (code-layout) order.
        let mut debug_offsets: BTreeMap<MethodRef, u32> = BTreeMap::new();
        let mut debug_dedup: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
        for (mref, di) in &debug_for_code {
            let bytes = self.encode_debug_info(di);
            let doff = if let Some(&existing) = debug_dedup.get(&bytes) {
                existing
            } else {
                let off = data.pos();
                data.put_bytes(&bytes);
                debug_dedup.insert(bytes, off);
                off
            };
            debug_offsets.insert(mref.clone(), doff);
        }
        for (mref, coff) in &method_code_off {
            if let Some(doff) = debug_offsets.get(mref) {
                data.patch_u32((*coff as usize) + 8, *doff);
            }
        }

        // 3. type_lists (align 4)
        let mut type_list_off: BTreeMap<Vec<String>, u32> = BTreeMap::new();
        for tl in &self.type_lists {
            let o = data.align(4);
            type_list_off.insert(tl.clone(), o);
            data.put_u32(tl.len() as u32);
            for t in tl {
                data.put_u16(self.type_idx[t] as u16);
            }
        }

        // 4. string_data (no align)
        let mut string_data_off: Vec<u32> = vec![0; self.strings.len()];
        for (i, s) in self.strings.iter().enumerate() {
            string_data_off[i] = data.pos();
            data.put_uleb128(mutf8::utf16_units(s));
            data.put_bytes(&mutf8::encode(s));
            data.put_u8(0);
        }

        // (5 annotation, 6 class_data, 7 encoded_array, 8 annotation_set, …)
        // 6. class_data (no align)
        let mut class_data_off: Vec<u32> = vec![0; file.classes.len()];
        for (ci, c) in file.classes.iter().enumerate() {
            if class_has_data(c) {
                class_data_off[ci] = data.pos();
                self.write_class_data(&mut data, c, &method_code_off);
            }
        }

        // 8. annotation_set (align 4): d8 always materializes one empty
        //    annotation_set_item (DexAnnotationSet.empty singleton) per DEX.
        let annotation_set_off = data.align(4);
        data.put_u32(0); // empty annotation_set_item (size 0)

        // map_list (align 4)
        let map_off = data.align(4);
        let map = self.build_map(
            file,
            string_ids_off,
            type_ids_off,
            proto_ids_off,
            field_ids_off,
            method_ids_off,
            class_defs_off,
            &code_offsets,
            &debug_offsets,
            &type_list_off,
            string_data_off[0],
            &class_data_off,
            annotation_set_off,
            map_off,
        );
        data.put_bytes(&map);

        let data_bytes = data.into_bytes();
        let data_size = data_bytes.len();
        let file_size = data_off + data_size;

        // ── Assemble the whole file. ──
        let mut out = vec![0u8; file_size];
        out[data_off..].copy_from_slice(&data_bytes);

        // string_ids
        for (i, &o) in string_data_off.iter().enumerate() {
            put_u32(&mut out, string_ids_off + i * 4, o);
        }
        // type_ids
        for (i, t) in self.types.iter().enumerate() {
            put_u32(&mut out, type_ids_off + i * 4, self.string_idx[t]);
        }
        // proto_ids
        for (i, p) in self.protos.iter().enumerate() {
            let base = proto_ids_off + i * 12;
            put_u32(&mut out, base, self.string_idx[&p.shorty()]);
            put_u32(&mut out, base + 4, self.type_idx[&p.return_type]);
            let params_off = if p.params.is_empty() {
                0
            } else {
                type_list_off[&p.params]
            };
            put_u32(&mut out, base + 8, params_off);
        }
        // field_ids
        for (i, f) in self.fields.iter().enumerate() {
            let base = field_ids_off + i * 8;
            put_u16(&mut out, base, self.type_idx[&f.class] as u16);
            put_u16(&mut out, base + 2, self.type_idx[&f.type_] as u16);
            put_u32(&mut out, base + 4, self.string_idx[&f.name]);
        }
        // method_ids
        for (i, m) in self.methods.iter().enumerate() {
            let base = method_ids_off + i * 8;
            put_u16(&mut out, base, self.type_idx[&m.class] as u16);
            put_u16(&mut out, base + 2, self.proto_idx[&m.proto] as u16);
            put_u32(&mut out, base + 4, self.string_idx[&m.name]);
        }
        // class_defs
        for (ci, c) in file.classes.iter().enumerate() {
            let base = class_defs_off + ci * 32;
            put_u32(&mut out, base, self.type_idx[&c.class_type]);
            put_u32(&mut out, base + 4, c.access_flags);
            put_u32(
                &mut out,
                base + 8,
                c.superclass.as_ref().map(|s| self.type_idx[s]).unwrap_or(NO_INDEX),
            );
            let interfaces_off = if c.interfaces.is_empty() {
                0
            } else {
                type_list_off[&c.interfaces]
            };
            put_u32(&mut out, base + 12, interfaces_off);
            put_u32(
                &mut out,
                base + 16,
                c.source_file.as_ref().map(|s| self.string_idx[s]).unwrap_or(NO_INDEX),
            );
            put_u32(&mut out, base + 20, 0); // annotations_off (TODO)
            put_u32(&mut out, base + 24, class_data_off[ci]);
            put_u32(&mut out, base + 28, 0); // static_values_off (TODO)
        }

        // header
        write_header(
            &mut out,
            file_size,
            map_off,
            string_ids_off,
            self.strings.len(),
            type_ids_off,
            self.types.len(),
            proto_ids_off,
            self.protos.len(),
            field_ids_off,
            self.fields.len(),
            method_ids_off,
            self.methods.len(),
            class_defs_off,
            file.classes.len(),
            data_off,
            data_size,
        );

        finalize_checksums(&mut out);
        out
    }

    fn write_code_item(&self, data: &mut DataSection, code: &CodeItem, has_debug: bool) {
        data.put_u16(code.registers_size);
        data.put_u16(code.ins_size);
        data.put_u16(code.outs_size);
        data.put_u16(code.tries.len() as u16);
        data.put_u32(0); // debug_info_off (patched later if has_debug)
        let _ = has_debug;
        data.put_u32(code.insns.len() as u32);
        // instructions with operands patched
        let insns_start = data.pos() as usize;
        for &unit in &code.insns {
            data.put_u16(unit);
        }
        for fx in &code.fixups {
            let pos = insns_start + fx.unit * 2;
            let idx = self.resolve(&fx.item);
            data.patch_u16(pos, idx as u16);
            if fx.wide {
                data.patch_u16(pos + 2, (idx >> 16) as u16);
            }
        }
        // tries (4-byte aligned if present and insns odd count of units)
        if !code.tries.is_empty() {
            if code.insns.len() % 2 == 1 {
                data.put_u16(0); // padding to 4-byte align
            }
            // Build handler lists first (encoded_catch_handler_list).
            let handlers_buf_start = data.pos();
            let _ = handlers_buf_start;
            // Each try_item: start_addr u32, insn_count u16, handler_off u16.
            // For simplicity, emit a handler list per try (no sharing).
            let mut handler_offsets: Vec<u16> = Vec::new();
            let mut handler_data = Vec::new();
            // handler list starts with count uleb
            write_uleb128(&mut handler_data, code.tries.len() as u32);
            for t in &code.tries {
                handler_offsets.push(handler_data.len() as u16);
                let size = t.handlers.len() as i32 * if t.catch_all_addr.is_some() { -1 } else { 1 };
                write_sleb128(&mut handler_data, size);
                for h in &t.handlers {
                    write_uleb128(&mut handler_data, self.type_idx[&h.exception_type]);
                    write_uleb128(&mut handler_data, h.addr);
                }
                if let Some(ca) = t.catch_all_addr {
                    write_uleb128(&mut handler_data, ca);
                }
            }
            for (i, t) in code.tries.iter().enumerate() {
                data.put_u32(t.start_addr);
                data.put_u16(t.insn_count);
                data.put_u16(handler_offsets[i]);
            }
            data.put_bytes(&handler_data);
        }
    }

    /// Encodes a `debug_info_item` to its byte form (pool indices resolved).
    /// Returned bytes are the canonicalization key: d8 shares one item between
    /// methods whose encoded debug info is identical (e.g. two `<init>`s).
    fn encode_debug_info(&self, di: &DebugInfo) -> Vec<u8> {
        let mut buf = Vec::new();
        write_uleb128(&mut buf, di.line_start);
        write_uleb128(&mut buf, di.parameter_names.len() as u32);
        for name in &di.parameter_names {
            match name {
                Some(n) => write_uleb128p1(&mut buf, self.string_idx[n] as i32),
                None => write_uleb128p1(&mut buf, -1),
            }
        }
        for ev in &di.events {
            self.write_debug_event(&mut buf, ev);
        }
        buf.push(0x00); // DBG_END_SEQUENCE
        buf
    }

    fn write_debug_event(&self, buf: &mut Vec<u8>, ev: &DebugEvent) {
        match ev {
            DebugEvent::AdvancePc { addr_diff } => {
                buf.push(0x01);
                write_uleb128(buf, *addr_diff);
            }
            DebugEvent::AdvanceLine { line_diff } => {
                buf.push(0x02);
                write_sleb128(buf, *line_diff);
            }
            DebugEvent::StartLocal { register, name, type_ } => {
                buf.push(0x03);
                write_uleb128(buf, *register);
                write_uleb128p1(buf, name.as_ref().map(|n| self.string_idx[n] as i32).unwrap_or(-1));
                write_uleb128p1(buf, type_.as_ref().map(|t| self.type_idx[t] as i32).unwrap_or(-1));
            }
            DebugEvent::StartLocalExtended { register, name, type_, sig } => {
                buf.push(0x04);
                write_uleb128(buf, *register);
                write_uleb128p1(buf, name.as_ref().map(|n| self.string_idx[n] as i32).unwrap_or(-1));
                write_uleb128p1(buf, type_.as_ref().map(|t| self.type_idx[t] as i32).unwrap_or(-1));
                write_uleb128p1(buf, sig.as_ref().map(|s| self.string_idx[s] as i32).unwrap_or(-1));
            }
            DebugEvent::EndLocal { register } => {
                buf.push(0x05);
                write_uleb128(buf, *register);
            }
            DebugEvent::RestartLocal { register } => {
                buf.push(0x06);
                write_uleb128(buf, *register);
            }
            DebugEvent::SetPrologueEnd => buf.push(0x07),
            DebugEvent::SetEpilogueBegin => buf.push(0x08),
            DebugEvent::SetFile { name } => {
                buf.push(0x09);
                write_uleb128p1(buf, name.as_ref().map(|n| self.string_idx[n] as i32).unwrap_or(-1));
            }
            DebugEvent::Special(op) => buf.push(*op),
        }
    }

    fn write_class_data(
        &self,
        data: &mut DataSection,
        c: &ClassDef,
        method_code_off: &BTreeMap<MethodRef, u32>,
    ) {
        let mut buf = Vec::new();
        write_uleb128(&mut buf, c.static_fields.len() as u32);
        write_uleb128(&mut buf, c.instance_fields.len() as u32);
        write_uleb128(&mut buf, c.direct_methods.len() as u32);
        write_uleb128(&mut buf, c.virtual_methods.len() as u32);
        self.write_encoded_fields(&mut buf, &c.static_fields);
        self.write_encoded_fields(&mut buf, &c.instance_fields);
        self.write_encoded_methods(&mut buf, &c.direct_methods, method_code_off);
        self.write_encoded_methods(&mut buf, &c.virtual_methods, method_code_off);
        data.put_bytes(&buf);
    }

    fn write_encoded_fields(&self, buf: &mut Vec<u8>, fields: &[EncodedField]) {
        // DEX requires encoded fields in ascending field-index order.
        let mut sorted: Vec<&EncodedField> = fields.iter().collect();
        sorted.sort_by_key(|ef| self.field_idx[&ef.field]);
        let mut prev = 0u32;
        for ef in sorted {
            let idx = self.field_idx[&ef.field];
            write_uleb128(buf, idx - prev);
            write_uleb128(buf, ef.access_flags);
            prev = idx;
        }
    }

    fn write_encoded_methods(
        &self,
        buf: &mut Vec<u8>,
        methods: &[EncodedMethod],
        method_code_off: &BTreeMap<MethodRef, u32>,
    ) {
        // DEX requires encoded methods in ascending method-index order.
        let mut sorted: Vec<&EncodedMethod> = methods.iter().collect();
        sorted.sort_by_key(|em| self.method_idx[&em.method]);
        let mut prev = 0u32;
        for em in sorted {
            let idx = self.method_idx[&em.method];
            write_uleb128(buf, idx - prev);
            write_uleb128(buf, em.access_flags);
            let coff = method_code_off.get(&em.method).copied().unwrap_or(0);
            write_uleb128(buf, coff);
            prev = idx;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_map(
        &self,
        file: &DexFile,
        string_ids_off: usize,
        type_ids_off: usize,
        proto_ids_off: usize,
        field_ids_off: usize,
        method_ids_off: usize,
        class_defs_off: usize,
        code_offsets: &[u32],
        debug_offsets: &BTreeMap<MethodRef, u32>,
        type_list_off: &BTreeMap<Vec<String>, u32>,
        first_string_data_off: u32,
        class_data_off: &[u32],
        annotation_set_off: u32,
        map_off: u32,
    ) -> Vec<u8> {
        let mut entries: Vec<(u16, u32, u32)> = Vec::new();
        entries.push((0x0000, 1, 0)); // header
        entries.push((0x0001, self.strings.len() as u32, string_ids_off as u32));
        entries.push((0x0002, self.types.len() as u32, type_ids_off as u32));
        if !self.protos.is_empty() {
            entries.push((0x0003, self.protos.len() as u32, proto_ids_off as u32));
        }
        if !self.fields.is_empty() {
            entries.push((0x0004, self.fields.len() as u32, field_ids_off as u32));
        }
        if !self.methods.is_empty() {
            entries.push((0x0005, self.methods.len() as u32, method_ids_off as u32));
        }
        entries.push((0x0006, file.classes.len() as u32, class_defs_off as u32));
        if !code_offsets.is_empty() {
            entries.push((0x2001, code_offsets.len() as u32, code_offsets[0]));
        }
        if !debug_offsets.is_empty() {
            let first = *debug_offsets.values().min().unwrap();
            // Count DISTINCT debug_info_item offsets — d8 shares one item across
            // methods with identical debug info, so method count != item count.
            let distinct: std::collections::BTreeSet<u32> = debug_offsets.values().copied().collect();
            entries.push((0x2003, distinct.len() as u32, first));
        }
        if !type_list_off.is_empty() {
            let first = *type_list_off.values().min().unwrap();
            entries.push((0x1001, type_list_off.len() as u32, first));
        }
        entries.push((0x2002, self.strings.len() as u32, first_string_data_off));
        let class_data_count = class_data_off.iter().filter(|&&o| o != 0).count();
        if class_data_count > 0 {
            let first = *class_data_off.iter().filter(|&&o| o != 0).min().unwrap();
            entries.push((0x2000, class_data_count as u32, first));
        }
        entries.push((0x1003, 1, annotation_set_off)); // empty annotation set
        entries.push((0x1000, 1, map_off));

        // Map must be sorted by offset.
        entries.sort_by_key(|e| e.2);
        let mut buf = Vec::new();
        push_u32(&mut buf, entries.len() as u32);
        for (t, size, off) in entries {
            push_u16(&mut buf, t);
            push_u16(&mut buf, 0); // unused
            push_u32(&mut buf, size);
            push_u32(&mut buf, off);
        }
        buf
    }
}

const NO_INDEX: u32 = 0xffff_ffff;

fn class_has_data(c: &ClassDef) -> bool {
    !c.static_fields.is_empty()
        || !c.instance_fields.is_empty()
        || !c.direct_methods.is_empty()
        || !c.virtual_methods.is_empty()
}

// ── data-section buffer ─────────────────────────────────────────────────────

struct DataSection {
    base: usize,
    buf: Vec<u8>,
}

impl DataSection {
    fn new(base: usize) -> DataSection {
        DataSection { base, buf: Vec::new() }
    }
    fn pos(&self) -> u32 {
        (self.base + self.buf.len()) as u32
    }
    fn align(&mut self, a: usize) -> u32 {
        while (self.base + self.buf.len()) % a != 0 {
            self.buf.push(0);
        }
        self.pos()
    }
    fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn put_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn put_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn put_bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    fn put_uleb128(&mut self, v: u32) {
        write_uleb128(&mut self.buf, v);
    }
    fn patch_u16(&mut self, abs_pos: usize, v: u16) {
        let rel = abs_pos - self.base;
        self.buf[rel..rel + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn patch_u32(&mut self, abs_pos: usize, v: u32) {
        let rel = abs_pos - self.base;
        self.buf[rel..rel + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn index_map<T: Clone + Ord>(items: &[T]) -> BTreeMap<T, u32> {
    items.iter().cloned().enumerate().map(|(i, t)| (t, i as u32)).collect()
}

fn cmp_proto(a: &ProtoRef, b: &ProtoRef, type_idx: &BTreeMap<String, u32>) -> std::cmp::Ordering {
    type_idx[&a.return_type]
        .cmp(&type_idx[&b.return_type])
        .then_with(|| cmp_type_list(&a.params, &b.params, type_idx))
}

fn cmp_type_list(a: &[String], b: &[String], type_idx: &BTreeMap<String, u32>) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        match type_idx[x].cmp(&type_idx[y]) {
            std::cmp::Ordering::Equal => {}
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

fn cmp_field(
    a: &FieldRef,
    b: &FieldRef,
    type_idx: &BTreeMap<String, u32>,
    string_idx: &BTreeMap<String, u32>,
) -> std::cmp::Ordering {
    type_idx[&a.class]
        .cmp(&type_idx[&b.class])
        .then_with(|| string_idx[&a.name].cmp(&string_idx[&b.name]))
        .then_with(|| type_idx[&a.type_].cmp(&type_idx[&b.type_]))
}

fn cmp_method(
    a: &MethodRef,
    b: &MethodRef,
    type_idx: &BTreeMap<String, u32>,
    string_idx: &BTreeMap<String, u32>,
    proto_idx: &BTreeMap<ProtoRef, u32>,
) -> std::cmp::Ordering {
    type_idx[&a.class]
        .cmp(&type_idx[&b.class])
        .then_with(|| string_idx[&a.name].cmp(&string_idx[&b.name]))
        .then_with(|| proto_idx[&a.proto].cmp(&proto_idx[&b.proto]))
}

/// d8's code-layout sort key: `holder.toSourceString() + MethodSignature`,
/// where the signature is `returnSource + ' ' + name + '(' + paramSources + ')'`
/// (see `DefaultMixedSectionLayoutStrategy.getKeyForDexCodeSorting` and
/// `MemberNaming.MethodSignature.toString`). Sorting by this string orders codes
/// by (holder, return type, name, params) — return type first.
fn code_sort_key(m: &MethodRef) -> String {
    let class = descriptor_to_source(&m.class);
    let ret = descriptor_to_source(&m.proto.return_type);
    let params: Vec<String> = m.proto.params.iter().map(|p| descriptor_to_source(p)).collect();
    format!("{class}{ret} {}({})", m.name, params.join(","))
}

/// JVM type descriptor → r8 `toSourceString()` form: `I`→"int", `V`→"void",
/// `[I`→"int[]", `Ljava/lang/String;`→"java.lang.String".
fn descriptor_to_source(desc: &str) -> String {
    let bytes = desc.as_bytes();
    let mut i = 0;
    let mut dims = 0;
    while i < bytes.len() && bytes[i] == b'[' {
        dims += 1;
        i += 1;
    }
    let mut s = match bytes.get(i) {
        Some(b'V') => "void".to_string(),
        Some(b'Z') => "boolean".to_string(),
        Some(b'B') => "byte".to_string(),
        Some(b'S') => "short".to_string(),
        Some(b'C') => "char".to_string(),
        Some(b'I') => "int".to_string(),
        Some(b'J') => "long".to_string(),
        Some(b'F') => "float".to_string(),
        Some(b'D') => "double".to_string(),
        Some(b'L') => desc[i + 1..desc.len() - 1].replace('/', "."),
        _ => desc[i..].to_string(),
    };
    for _ in 0..dims {
        s.push_str("[]");
    }
    s
}

fn collect_field(
    f: &FieldRef,
    strings: &mut std::collections::BTreeSet<String>,
    types: &mut std::collections::BTreeSet<String>,
    fields: &mut std::collections::BTreeSet<FieldRef>,
) {
    types.insert(f.class.clone());
    types.insert(f.type_.clone());
    strings.insert(f.class.clone());
    strings.insert(f.type_.clone());
    strings.insert(f.name.clone());
    fields.insert(f.clone());
}

fn collect_method(
    m: &MethodRef,
    strings: &mut std::collections::BTreeSet<String>,
    types: &mut std::collections::BTreeSet<String>,
    protos: &mut std::collections::BTreeSet<ProtoRef>,
    methods: &mut std::collections::BTreeSet<MethodRef>,
) {
    types.insert(m.class.clone());
    strings.insert(m.class.clone());
    strings.insert(m.name.clone());
    collect_proto(&m.proto, strings, types, protos);
    methods.insert(m.clone());
}

fn collect_proto(
    p: &ProtoRef,
    strings: &mut std::collections::BTreeSet<String>,
    types: &mut std::collections::BTreeSet<String>,
    protos: &mut std::collections::BTreeSet<ProtoRef>,
) {
    strings.insert(p.shorty());
    types.insert(p.return_type.clone());
    strings.insert(p.return_type.clone());
    for t in &p.params {
        types.insert(t.clone());
        strings.insert(t.clone());
    }
    protos.insert(p.clone());
}

fn collect_itemref(
    item: &ItemRef,
    strings: &mut std::collections::BTreeSet<String>,
    types: &mut std::collections::BTreeSet<String>,
    protos: &mut std::collections::BTreeSet<ProtoRef>,
    fields: &mut std::collections::BTreeSet<FieldRef>,
    methods: &mut std::collections::BTreeSet<MethodRef>,
) {
    match item {
        ItemRef::String(s) => {
            strings.insert(s.clone());
        }
        ItemRef::Type(t) => {
            types.insert(t.clone());
            strings.insert(t.clone());
        }
        ItemRef::Field(f) => collect_field(f, strings, types, fields),
        ItemRef::Method(m) => collect_method(m, strings, types, protos, methods),
        ItemRef::Proto(p) => collect_proto(p, strings, types, protos),
        ItemRef::CallSite(_) => {}
    }
}

fn collect_debug(
    di: &DebugInfo,
    strings: &mut std::collections::BTreeSet<String>,
    types: &mut std::collections::BTreeSet<String>,
) {
    for n in di.parameter_names.iter().flatten() {
        strings.insert(n.clone());
    }
    for ev in &di.events {
        match ev {
            DebugEvent::StartLocal { name, type_, .. }
            | DebugEvent::StartLocalExtended { name, type_, .. } => {
                if let Some(n) = name {
                    strings.insert(n.clone());
                }
                if let Some(t) = type_ {
                    types.insert(t.clone());
                    strings.insert(t.clone());
                }
            }
            DebugEvent::SetFile { name: Some(n) } => {
                strings.insert(n.clone());
            }
            _ => {}
        }
    }
}

fn put_u16(out: &mut [u8], pos: usize, v: u16) {
    out[pos..pos + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut [u8], pos: usize, v: u32) {
    out[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
}
fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[allow(clippy::too_many_arguments)]
fn write_header(
    out: &mut [u8],
    file_size: usize,
    map_off: u32,
    string_ids_off: usize,
    string_ids_size: usize,
    type_ids_off: usize,
    type_ids_size: usize,
    proto_ids_off: usize,
    proto_ids_size: usize,
    field_ids_off: usize,
    field_ids_size: usize,
    method_ids_off: usize,
    method_ids_size: usize,
    class_defs_off: usize,
    class_defs_size: usize,
    data_off: usize,
    data_size: usize,
) {
    out[0..8].copy_from_slice(b"dex\n035\0");
    // 0x08 checksum, 0x0c signature — filled in finalize.
    put_u32(out, 0x20, file_size as u32);
    put_u32(out, 0x24, HEADER_SIZE as u32);
    put_u32(out, 0x28, ENDIAN_TAG);
    put_u32(out, 0x2c, 0); // link_size
    put_u32(out, 0x30, 0); // link_off
    put_u32(out, 0x34, map_off);
    put_u32(out, 0x38, string_ids_size as u32);
    put_u32(out, 0x3c, if string_ids_size == 0 { 0 } else { string_ids_off as u32 });
    put_u32(out, 0x40, type_ids_size as u32);
    put_u32(out, 0x44, if type_ids_size == 0 { 0 } else { type_ids_off as u32 });
    put_u32(out, 0x48, proto_ids_size as u32);
    put_u32(out, 0x4c, if proto_ids_size == 0 { 0 } else { proto_ids_off as u32 });
    put_u32(out, 0x50, field_ids_size as u32);
    put_u32(out, 0x54, field_ids_off as u32);
    put_u32(out, 0x58, method_ids_size as u32);
    put_u32(out, 0x5c, if method_ids_size == 0 { 0 } else { method_ids_off as u32 });
    put_u32(out, 0x60, class_defs_size as u32);
    put_u32(out, 0x64, if class_defs_size == 0 { 0 } else { class_defs_off as u32 });
    put_u32(out, 0x68, data_size as u32);
    put_u32(out, 0x6c, data_off as u32);
}

/// Computes the SHA-1 signature (over everything after the signature field) and
/// the Adler-32 checksum (over everything after the checksum field).
fn finalize_checksums(out: &mut [u8]) {
    use sha1::{Digest, Sha1};
    let sig = Sha1::digest(&out[0x20..]);
    out[0x0c..0x20].copy_from_slice(&sig);
    let checksum = adler::adler32_slice(&out[0x0c..]);
    put_u32(out, 0x08, checksum);
}
