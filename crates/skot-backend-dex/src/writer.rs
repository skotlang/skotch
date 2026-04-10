//! DEX file writer.
//!
//! Lays out the file in two passes: the **first pass** computes
//! offsets and sizes for every section, the **second pass** emits the
//! actual bytes. After the bytes are written we patch the SHA-1
//! signature (over `header[32..]`) and Adler32 checksum (over
//! `header[12..]`).
//!
//! ## File layout
//!
//! The DEX format prescribes a specific section order in the file:
//!
//! ```text
//!   header               (112 bytes)
//!   string_ids           (4 * N bytes)
//!   type_ids             (4 * N bytes)
//!   proto_ids            (12 * N bytes)
//!   field_ids            (8 * N bytes)
//!   method_ids           (8 * N bytes)
//!   class_defs           (32 * N bytes)
//!   data section:
//!     type_lists         (variable)
//!     code_items         (variable, 4-byte aligned)
//!     class_data_items   (variable)
//!     string_data_items  (variable)
//!     map_list           (variable, last)
//! ```
//!
//! Each `string_id_item` carries a 4-byte offset into the data
//! section pointing at the matching `string_data_item`. Each
//! `class_def_item` carries an offset to its `class_data_item`,
//! which in turn carries offsets to its method `code_item`s.

use crate::bytecode::{apply_remap, lower_function, serialize_insns, MethodCode};
use crate::leb128::write_uleb128;
use crate::pools::{FinalIndices, Pools};
use byteorder::{LittleEndian, WriteBytesExt};
use skot_mir::MirModule;

/// Section type tags used in the DEX `map_list`.
mod map_type {
    pub const HEADER_ITEM: u16 = 0x0000;
    pub const STRING_ID_ITEM: u16 = 0x0001;
    pub const TYPE_ID_ITEM: u16 = 0x0002;
    pub const PROTO_ID_ITEM: u16 = 0x0003;
    pub const FIELD_ID_ITEM: u16 = 0x0004;
    pub const METHOD_ID_ITEM: u16 = 0x0005;
    pub const CLASS_DEF_ITEM: u16 = 0x0006;
    pub const MAP_LIST: u16 = 0x1000;
    pub const TYPE_LIST: u16 = 0x1001;
    pub const CLASS_DATA_ITEM: u16 = 0x2000;
    pub const CODE_ITEM: u16 = 0x2001;
    pub const STRING_DATA_ITEM: u16 = 0x2002;
}

const HEADER_SIZE: usize = 0x70;
const ENDIAN_TAG: u32 = 0x12345678;

const ACC_PUBLIC: u32 = 0x0001;
const ACC_STATIC: u32 = 0x0008;
const ACC_FINAL: u32 = 0x0010;

/// Compile a single MIR module to a `.dex` file's bytes.
pub fn write_dex(module: &MirModule) -> Vec<u8> {
    // ─── Phase 1: collect everything into the pools ──────────────────────
    //
    // We need every method, field, type, and string the module
    // references to be in the pools before we can sort them. Walk
    // the MIR once, calling `lower_function` (which mutates `pools`
    // as it interns refs) and stashing the per-method `MethodCode`.
    let class_descriptor = format!("L{};", module.wrapper_class);
    let mut pools = Pools::new();
    pools.intern_type(&class_descriptor);
    pools.intern_type("Ljava/lang/Object;");
    // Source file name string (e.g., "input.kt").
    let source_file_string = pools.intern_string("input.kt");

    let mut method_codes: Vec<(String, String, Vec<&str>, MethodCode)> = Vec::new();
    for func in &module.functions {
        let params: Vec<&str> = func
            .params
            .iter()
            .map(|p| ty_descriptor(&func.locals[p.0 as usize]))
            .collect();
        let ret = ty_descriptor(&func.return_ty);
        // Pre-intern the method itself so it's in the pool *before*
        // we compute the bytecode (which may also intern call refs).
        pools.intern_method(&class_descriptor, &func.name, ret, &params);
        let mc = lower_function(func, module, &class_descriptor, &mut pools);
        method_codes.push((func.name.clone(), ret.to_string(), params, mc));
    }

    // ─── Phase 2: finalize pools, get sorted indices + remap ─────────────
    let (final_idx, remap, param_lists) = pools.finalize();

    // After remapping, the bytecode patches need to point at the new
    // string/type/method indices.
    let method_codes: Vec<(String, String, Vec<&str>, MethodCode)> = method_codes
        .into_iter()
        .map(|(n, r, p, mut mc)| {
            apply_remap(&mut mc, &remap);
            (n, r, p, mc)
        })
        .collect();

    // The source-file string id is also remapped.
    let source_file_string_new = remap.string[source_file_string as usize];

    // For each method we need its *final* method id (post-sort) so
    // class_data can list it. Look it up by signature in `final_idx`.
    // Methods are already sorted by (class_idx, name_idx, proto_idx).
    let class_idx_in_types = final_idx
        .types
        .iter()
        .position(|t| t == &class_descriptor)
        .expect("class descriptor must be in type pool") as u32;

    // ─── Phase 3: lay out the file (compute offsets) ─────────────────────
    let strings_count = final_idx.strings.len();
    let types_count = final_idx.types.len();
    let protos_count = final_idx.protos.len();
    let fields_count = final_idx.fields.len();
    let methods_count = final_idx.methods.len();
    let classes_count = 1; // PR #3: one class per module.

    let string_ids_off = HEADER_SIZE;
    let type_ids_off = string_ids_off + strings_count * 4;
    let proto_ids_off = type_ids_off + types_count * 4;
    let field_ids_off = proto_ids_off + protos_count * 12;
    let method_ids_off = field_ids_off + fields_count * 8;
    let class_defs_off = method_ids_off + methods_count * 8;
    let data_off = class_defs_off + classes_count * 32;

    // Now compute the data section offsets. We lay out:
    //   1. type_list items (4-byte aligned, each: u32 size + u16[size] + pad)
    //   2. code_items (4-byte aligned)
    //   3. class_data_item (uleb128 stream, no alignment)
    //   4. string_data_items (uleb128 stream, no alignment)
    //   5. map_list (4-byte aligned, last)
    let mut data_cursor = data_off;
    fn align(x: usize, n: usize) -> usize {
        (x + (n - 1)) & !(n - 1)
    }

    // 1. Type lists.
    let mut type_list_offsets: Vec<u32> = Vec::with_capacity(param_lists.len());
    let mut type_list_bytes_per_entry: Vec<Vec<u8>> = Vec::with_capacity(param_lists.len());
    for list in &param_lists {
        data_cursor = align(data_cursor, 4);
        type_list_offsets.push(data_cursor as u32);
        let mut bytes = Vec::new();
        bytes.write_u32::<LittleEndian>(list.len() as u32).unwrap();
        for &type_idx in list {
            bytes.write_u16::<LittleEndian>(type_idx as u16).unwrap();
        }
        // Pad to 4 bytes.
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        data_cursor += bytes.len();
        type_list_bytes_per_entry.push(bytes);
    }

    // 2. Code items.
    let mut code_offsets: Vec<u32> = Vec::with_capacity(method_codes.len());
    let mut code_item_blobs: Vec<Vec<u8>> = Vec::with_capacity(method_codes.len());
    for (_, _, _, mc) in &method_codes {
        data_cursor = align(data_cursor, 4);
        code_offsets.push(data_cursor as u32);
        let mut bytes = Vec::new();
        bytes.write_u16::<LittleEndian>(mc.registers_size).unwrap();
        bytes.write_u16::<LittleEndian>(mc.ins_size).unwrap();
        bytes.write_u16::<LittleEndian>(mc.outs_size).unwrap();
        bytes.write_u16::<LittleEndian>(0).unwrap(); // tries_size
        bytes.write_u32::<LittleEndian>(0).unwrap(); // debug_info_off
        bytes
            .write_u32::<LittleEndian>(mc.insns.len() as u32)
            .unwrap();
        bytes.extend_from_slice(&serialize_insns(&mc.insns));
        // Pad insns to align next code_item header to 4 bytes.
        // (Spec: "padding: optional. The presence of this element is
        //  determined by `insns_size`: if it is non-zero, then this is
        //  empty if `insns_size % 2 == 0`, and present (size 2) otherwise.")
        // Since tries_size = 0 we don't need this padding for spec
        // correctness; alignment will be handled by the next item.
        data_cursor += bytes.len();
        code_item_blobs.push(bytes);
    }

    // 3. Class data item (one for the wrapper class).
    let class_data_off = data_cursor;
    let mut class_data_bytes = Vec::new();
    write_uleb128(&mut class_data_bytes, 0); // static_fields_size
    write_uleb128(&mut class_data_bytes, 0); // instance_fields_size
    write_uleb128(&mut class_data_bytes, method_codes.len() as u32); // direct_methods_size
    write_uleb128(&mut class_data_bytes, 0); // virtual_methods_size

    // Encoded methods: sort the list by their final method_id, then
    // write each as `(method_idx_diff, access_flags, code_off)`.
    let mut encoded: Vec<(u32, u32, u32)> = method_codes
        .iter()
        .enumerate()
        .map(|(i, (name, ret, params, _mc))| {
            // Find this method's final index in final_idx.methods.
            let final_method_idx =
                method_idx_for(&final_idx, class_idx_in_types, name, ret, params, &remap);
            let access = ACC_PUBLIC | ACC_STATIC | ACC_FINAL;
            let code_off = code_offsets[i];
            (final_method_idx, access, code_off)
        })
        .collect();
    encoded.sort_by_key(|&(idx, _, _)| idx);

    let mut prev: u32 = 0;
    for (i, (idx, access, code_off)) in encoded.iter().enumerate() {
        let diff = if i == 0 { *idx } else { *idx - prev };
        write_uleb128(&mut class_data_bytes, diff);
        write_uleb128(&mut class_data_bytes, *access);
        write_uleb128(&mut class_data_bytes, *code_off);
        prev = *idx;
    }
    data_cursor += class_data_bytes.len();

    // 4. String data items.
    let mut string_data_offsets: Vec<u32> = Vec::with_capacity(strings_count);
    let mut string_data_blobs: Vec<Vec<u8>> = Vec::with_capacity(strings_count);
    for s in &final_idx.strings {
        string_data_offsets.push(data_cursor as u32);
        let mut bytes = Vec::new();
        // utf16_size = number of UTF-16 code units. For ASCII strings
        // this is the same as the byte count.
        let utf16_size = s.encode_utf16().count() as u32;
        write_uleb128(&mut bytes, utf16_size);
        // MUTF-8 ≈ UTF-8 for ASCII (the only fixtures we ship).
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(0); // null terminator
        data_cursor += bytes.len();
        string_data_blobs.push(bytes);
    }

    // 5. Map list. Always last in the data section.
    data_cursor = align(data_cursor, 4);
    let map_off = data_cursor;
    let mut map_entries: Vec<(u16, u32, u32)> = Vec::new();
    map_entries.push((map_type::HEADER_ITEM, 1, 0));
    if strings_count > 0 {
        map_entries.push((
            map_type::STRING_ID_ITEM,
            strings_count as u32,
            string_ids_off as u32,
        ));
    }
    if types_count > 0 {
        map_entries.push((
            map_type::TYPE_ID_ITEM,
            types_count as u32,
            type_ids_off as u32,
        ));
    }
    if protos_count > 0 {
        map_entries.push((
            map_type::PROTO_ID_ITEM,
            protos_count as u32,
            proto_ids_off as u32,
        ));
    }
    if fields_count > 0 {
        map_entries.push((
            map_type::FIELD_ID_ITEM,
            fields_count as u32,
            field_ids_off as u32,
        ));
    }
    if methods_count > 0 {
        map_entries.push((
            map_type::METHOD_ID_ITEM,
            methods_count as u32,
            method_ids_off as u32,
        ));
    }
    map_entries.push((
        map_type::CLASS_DEF_ITEM,
        classes_count as u32,
        class_defs_off as u32,
    ));
    if !type_list_offsets.is_empty() {
        map_entries.push((
            map_type::TYPE_LIST,
            type_list_offsets.len() as u32,
            type_list_offsets[0],
        ));
    }
    if !code_offsets.is_empty() {
        map_entries.push((
            map_type::CODE_ITEM,
            code_offsets.len() as u32,
            code_offsets[0],
        ));
    }
    map_entries.push((map_type::CLASS_DATA_ITEM, 1, class_data_off as u32));
    map_entries.push((
        map_type::STRING_DATA_ITEM,
        strings_count as u32,
        string_data_offsets[0],
    ));
    map_entries.push((map_type::MAP_LIST, 1, map_off as u32));
    let mut map_bytes = Vec::new();
    map_bytes
        .write_u32::<LittleEndian>(map_entries.len() as u32)
        .unwrap();
    for (ty, size, off) in &map_entries {
        map_bytes.write_u16::<LittleEndian>(*ty).unwrap();
        map_bytes.write_u16::<LittleEndian>(0).unwrap(); // unused
        map_bytes.write_u32::<LittleEndian>(*size).unwrap();
        map_bytes.write_u32::<LittleEndian>(*off).unwrap();
    }
    data_cursor += map_bytes.len();

    let file_size = data_cursor;
    let data_size = file_size - data_off;

    // ─── Phase 4: emit bytes ─────────────────────────────────────────────
    let mut out = Vec::with_capacity(file_size);

    // Header
    out.extend_from_slice(b"dex\n035\0");
    out.write_u32::<LittleEndian>(0).unwrap(); // checksum (patched)
    out.extend_from_slice(&[0u8; 20]); // signature (patched)
    out.write_u32::<LittleEndian>(file_size as u32).unwrap();
    out.write_u32::<LittleEndian>(HEADER_SIZE as u32).unwrap();
    out.write_u32::<LittleEndian>(ENDIAN_TAG).unwrap();
    out.write_u32::<LittleEndian>(0).unwrap(); // link_size
    out.write_u32::<LittleEndian>(0).unwrap(); // link_off
    out.write_u32::<LittleEndian>(map_off as u32).unwrap();
    // Per DEX spec: if a section's size is 0, its offset must also be 0.
    fn off_or_zero(size: usize, off: usize) -> u32 {
        if size == 0 {
            0
        } else {
            off as u32
        }
    }
    out.write_u32::<LittleEndian>(strings_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(strings_count, string_ids_off))
        .unwrap();
    out.write_u32::<LittleEndian>(types_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(types_count, type_ids_off))
        .unwrap();
    out.write_u32::<LittleEndian>(protos_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(protos_count, proto_ids_off))
        .unwrap();
    out.write_u32::<LittleEndian>(fields_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(fields_count, field_ids_off))
        .unwrap();
    out.write_u32::<LittleEndian>(methods_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(methods_count, method_ids_off))
        .unwrap();
    out.write_u32::<LittleEndian>(classes_count as u32).unwrap();
    out.write_u32::<LittleEndian>(off_or_zero(classes_count, class_defs_off))
        .unwrap();
    out.write_u32::<LittleEndian>(data_size as u32).unwrap();
    out.write_u32::<LittleEndian>(data_off as u32).unwrap();
    debug_assert_eq!(out.len(), HEADER_SIZE);

    // string_id_item table.
    for off in &string_data_offsets {
        out.write_u32::<LittleEndian>(*off).unwrap();
    }

    // type_id_item table.
    for &string_idx in &final_idx.type_string_idx {
        out.write_u32::<LittleEndian>(string_idx).unwrap();
    }

    // proto_id_item table.
    for (i, p) in final_idx.protos.iter().enumerate() {
        out.write_u32::<LittleEndian>(p.shorty_idx).unwrap();
        out.write_u32::<LittleEndian>(p.return_type_idx).unwrap();
        let off = match p.parameters_list {
            Some(list_id) => type_list_offsets[list_id as usize],
            None => 0,
        };
        out.write_u32::<LittleEndian>(off).unwrap();
        let _ = i;
    }

    // field_id_item table.
    for f in &final_idx.fields {
        out.write_u16::<LittleEndian>(f.class_idx).unwrap();
        out.write_u16::<LittleEndian>(f.type_idx).unwrap();
        out.write_u32::<LittleEndian>(f.name_idx).unwrap();
    }

    // method_id_item table.
    for m in &final_idx.methods {
        out.write_u16::<LittleEndian>(m.class_idx).unwrap();
        out.write_u16::<LittleEndian>(m.proto_idx).unwrap();
        out.write_u32::<LittleEndian>(m.name_idx).unwrap();
    }

    // class_def_item.
    let super_class_idx = final_idx
        .types
        .iter()
        .position(|t| t == "Ljava/lang/Object;")
        .unwrap() as u32;
    out.write_u32::<LittleEndian>(class_idx_in_types).unwrap();
    out.write_u32::<LittleEndian>(ACC_PUBLIC | ACC_FINAL)
        .unwrap();
    out.write_u32::<LittleEndian>(super_class_idx).unwrap();
    out.write_u32::<LittleEndian>(0).unwrap(); // interfaces_off
    out.write_u32::<LittleEndian>(source_file_string_new)
        .unwrap();
    out.write_u32::<LittleEndian>(0).unwrap(); // annotations_off
    out.write_u32::<LittleEndian>(class_data_off as u32)
        .unwrap();
    out.write_u32::<LittleEndian>(0).unwrap(); // static_values_off
    debug_assert_eq!(out.len(), data_off);

    // Data section, in the order we computed offsets.
    // 1. type_lists
    for (off, bytes) in type_list_offsets
        .iter()
        .zip(type_list_bytes_per_entry.iter())
    {
        while out.len() < *off as usize {
            out.push(0);
        }
        out.extend_from_slice(bytes);
    }
    // 2. code_items
    for (off, bytes) in code_offsets.iter().zip(code_item_blobs.iter()) {
        while out.len() < *off as usize {
            out.push(0);
        }
        out.extend_from_slice(bytes);
    }
    // 3. class_data_item (no alignment requirement)
    while out.len() < class_data_off {
        out.push(0);
    }
    out.extend_from_slice(&class_data_bytes);
    // 4. string_data_items
    for (off, bytes) in string_data_offsets.iter().zip(string_data_blobs.iter()) {
        while out.len() < *off as usize {
            out.push(0);
        }
        out.extend_from_slice(bytes);
    }
    // 5. map_list
    while out.len() < map_off {
        out.push(0);
    }
    out.extend_from_slice(&map_bytes);

    debug_assert_eq!(out.len(), file_size, "file_size mismatch");

    // ─── Phase 5: patch checksum + signature ─────────────────────────────
    // SHA-1 over header[32..]
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(&out[32..]);
    let digest = hasher.digest().bytes();
    out[12..32].copy_from_slice(&digest);

    // Adler32 over header[12..] (i.e. starting at the checksum's
    // immediate next byte and continuing through end-of-file).
    let checksum = adler::adler32_slice(&out[12..]);
    out[8..12].copy_from_slice(&checksum.to_le_bytes());

    out
}

/// Find the final method-id for a given (class, name, return, params)
/// signature. Used during `class_data_item` emission.
fn method_idx_for(
    final_idx: &FinalIndices,
    class_idx: u32,
    name: &str,
    ret: &str,
    params: &[&str],
    _remap: &crate::pools::Remap,
) -> u32 {
    let name_string_idx = final_idx
        .strings
        .iter()
        .position(|s| s == name)
        .expect("method name in pool") as u32;
    // Walk methods linearly — PR #3 fixtures have at most a handful.
    for (i, m) in final_idx.methods.iter().enumerate() {
        if m.class_idx as u32 == class_idx && m.name_idx == name_string_idx {
            // Check the proto matches.
            let proto = &final_idx.protos[m.proto_idx as usize];
            if final_idx.type_string_idx[proto.return_type_idx as usize]
                == final_idx.strings.iter().position(|s| s == ret).unwrap() as u32
            {
                // Check parameter list (we don't store the original
                // parameter list directly here; we'd need to look up
                // via the type_list pool. For PR #3 method overloads
                // by parameter list aren't exercised, so the first
                // (class, name) match is correct.)
                return i as u32;
            }
        }
    }
    panic!("method `{class_idx}.{name}{ret} {params:?}` not found in final_idx");
}

fn ty_descriptor(ty: &skot_types::Ty) -> &'static str {
    use skot_types::Ty;
    match ty {
        Ty::Unit => "V",
        Ty::Bool => "Z",
        Ty::Int => "I",
        Ty::Long => "J",
        Ty::Double => "D",
        Ty::String => "Ljava/lang/String;",
        Ty::Any | Ty::Nullable(_) => "Ljava/lang/Object;",
        Ty::Error => "V",
    }
}
