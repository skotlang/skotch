//! Index pools for the DEX file format.
//!
//! ## Why two passes
//!
//! DEX requires every index table to be **sorted** in a specific
//! order, and every other table that references it does so by index
//! into the sorted form. The string id table sorts by modified-UTF-8
//! byte order; the type id table sorts by string-id (so transitively
//! by string content); proto, field, and method tables sort by their
//! own derived keys.
//!
//! That means we cannot assign final indices while collecting
//! references. Instead we collect into insertion-order pools (deduped
//! by content), then run a single sort+remap pass that produces the
//! final tables and a set of "old → new" maps that the writer uses
//! when laying out the file.
//!
//! All references are stored *symbolically* in the pools — strings
//! are stored as `String`, types as the descriptor `String`, methods
//! as a `(class_descriptor, name, return_type, params)` tuple. Final
//! indices are computed only by [`Pools::finalize`].

use rustc_hash::FxHashMap;

/// Stable, post-finalization indices into the DEX index tables.
///
/// These are emitted by [`Pools::finalize`] and consumed by both the
/// writer (for the final layout) and the bytecode patcher (which
/// holds these indices in instructions placeholder slots).
#[derive(Debug, Default, Clone)]
pub struct FinalIndices {
    /// `strings[i]` is the i-th string in MUTF-8 byte order. The
    /// string's index in this `Vec` is its DEX `string_id`.
    pub strings: Vec<String>,
    /// `types[i]` is the i-th type descriptor; its DEX `type_id` is
    /// `i`. Each entry is a string in `strings`.
    pub types: Vec<String>,
    /// Final type-id of each type, indexed parallel to `types`. The
    /// outer `Vec` is the type id; the inner is the string-id.
    pub type_string_idx: Vec<u32>,
    /// `protos[i]` is `(shorty_string_idx, return_type_idx, parameters)`.
    pub protos: Vec<ProtoRow>,
    /// `fields[i]` is `(class_idx, type_idx, name_idx)`.
    pub fields: Vec<FieldRow>,
    /// `methods[i]` is `(class_idx, proto_idx, name_idx)`.
    pub methods: Vec<MethodRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtoRow {
    pub shorty_idx: u32,
    pub return_type_idx: u32,
    /// Index in `Pools::param_lists` (0 means "no parameters list").
    pub parameters_list: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldRow {
    pub class_idx: u16,
    pub type_idx: u16,
    pub name_idx: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodRow {
    pub class_idx: u16,
    pub proto_idx: u16,
    pub name_idx: u32,
}

/// Symbolic, deferred-index pool. Built up while walking the MIR;
/// finalized into [`FinalIndices`] after all references are known.
#[derive(Default)]
pub struct Pools {
    pub strings: Vec<String>,
    pub strings_dedup: FxHashMap<String, u32>,

    pub types: Vec<String>,
    pub types_dedup: FxHashMap<String, u32>,

    /// Each entry is `(return_type_descriptor, [param_descriptors])`.
    pub protos: Vec<(String, Vec<String>)>,
    pub protos_dedup: FxHashMap<(String, Vec<String>), u32>,

    /// Each entry is `(class_descriptor, name, type_descriptor)`.
    pub fields: Vec<(String, String, String)>,
    pub fields_dedup: FxHashMap<(String, String, String), u32>,

    /// Each entry is `(class_descriptor, name, return_type, [param_descriptors])`.
    pub methods: Vec<(String, String, String, Vec<String>)>,
    pub methods_dedup: FxHashMap<(String, String, String, Vec<String>), u32>,
}

impl Pools {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.strings_dedup.get(s) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.strings_dedup.insert(s.to_string(), idx);
        idx
    }

    pub fn intern_type(&mut self, descriptor: &str) -> u32 {
        if let Some(&idx) = self.types_dedup.get(descriptor) {
            return idx;
        }
        // Make sure the descriptor itself is in the string pool.
        self.intern_string(descriptor);
        let idx = self.types.len() as u32;
        self.types.push(descriptor.to_string());
        self.types_dedup.insert(descriptor.to_string(), idx);
        idx
    }

    pub fn intern_proto(&mut self, return_ty: &str, params: &[&str]) -> u32 {
        let key = (
            return_ty.to_string(),
            params.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
        );
        if let Some(&idx) = self.protos_dedup.get(&key) {
            return idx;
        }
        // Pre-intern the constituent strings/types so they end up in
        // their respective pools before finalize().
        self.intern_string(&shorty_for(return_ty, params));
        self.intern_type(return_ty);
        for p in params {
            self.intern_type(p);
        }
        let idx = self.protos.len() as u32;
        self.protos.push(key.clone());
        self.protos_dedup.insert(key, idx);
        idx
    }

    pub fn intern_field(&mut self, class_desc: &str, name: &str, type_desc: &str) -> u32 {
        let key = (
            class_desc.to_string(),
            name.to_string(),
            type_desc.to_string(),
        );
        if let Some(&idx) = self.fields_dedup.get(&key) {
            return idx;
        }
        self.intern_type(class_desc);
        self.intern_type(type_desc);
        self.intern_string(name);
        let idx = self.fields.len() as u32;
        self.fields.push(key.clone());
        self.fields_dedup.insert(key, idx);
        idx
    }

    pub fn intern_method(
        &mut self,
        class_desc: &str,
        name: &str,
        return_ty: &str,
        params: &[&str],
    ) -> u32 {
        let key = (
            class_desc.to_string(),
            name.to_string(),
            return_ty.to_string(),
            params.iter().map(|p| (*p).to_string()).collect::<Vec<_>>(),
        );
        if let Some(&idx) = self.methods_dedup.get(&key) {
            return idx;
        }
        self.intern_type(class_desc);
        self.intern_proto(return_ty, params);
        self.intern_string(name);
        let idx = self.methods.len() as u32;
        self.methods.push(key.clone());
        self.methods_dedup.insert(key, idx);
        idx
    }

    /// Sort all pools and return the final id tables plus remap maps.
    ///
    /// The remap maps are returned so the bytecode patcher can rewrite
    /// the placeholder string/type/method indices it stored in the
    /// instructions during the first pass.
    pub fn finalize(self) -> (FinalIndices, Remap, Vec<Vec<u32>>) {
        // 1. Sort strings by raw byte order. ASCII = MUTF-8 for the
        //    fixtures we ship.
        let mut strings = self.strings.clone();
        strings.sort();
        let mut string_remap_old_to_new = vec![0u32; strings.len()];
        let new_string_idx: FxHashMap<String, u32> = strings
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();
        for (old_i, s) in self.strings.iter().enumerate() {
            string_remap_old_to_new[old_i] = new_string_idx[s];
        }

        // 2. Sort types by their (new) string-id. We rebuild from
        //    `self.types` which is the descriptor list in collection
        //    order; sort by descriptor → new-string-id, which since
        //    strings are sorted is just the same as sorting by
        //    descriptor.
        let mut types = self.types.clone();
        types.sort();
        let new_type_idx: FxHashMap<String, u32> = types
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u32))
            .collect();
        let mut type_remap_old_to_new = vec![0u32; self.types.len()];
        for (old_i, t) in self.types.iter().enumerate() {
            type_remap_old_to_new[old_i] = new_type_idx[t];
        }
        let type_string_idx: Vec<u32> = types.iter().map(|t| new_string_idx[t]).collect();

        // 3. Sort protos by (return_type_idx, [param_type_idx...]).
        //    First convert each proto to its sorted-key form.
        let mut proto_keys: Vec<((u32, Vec<u32>), usize)> = self
            .protos
            .iter()
            .enumerate()
            .map(|(old_i, (ret, params))| {
                let ret_idx = new_type_idx[ret];
                let p_idx: Vec<u32> = params.iter().map(|p| new_type_idx[p]).collect();
                ((ret_idx, p_idx), old_i)
            })
            .collect();
        proto_keys.sort_by(|a, b| a.0.cmp(&b.0));

        // Build the parameter type-list pool. The DEX spec says each
        // distinct parameter list is stored once in `data` as a
        // `type_list`. Empty lists are not stored — proto's
        // `parameters_off` is 0 in that case.
        let mut param_lists: Vec<Vec<u32>> = Vec::new();
        let mut param_list_dedup: FxHashMap<Vec<u32>, u32> = FxHashMap::default();
        let mut protos: Vec<ProtoRow> = Vec::with_capacity(self.protos.len());
        let mut proto_remap_old_to_new = vec![0u32; self.protos.len()];
        for (new_i, ((ret_idx, p_idx), old_i)) in proto_keys.iter().enumerate() {
            let parameters_list = if p_idx.is_empty() {
                None
            } else if let Some(&existing) = param_list_dedup.get(p_idx) {
                Some(existing)
            } else {
                let id = param_lists.len() as u32;
                param_lists.push(p_idx.clone());
                param_list_dedup.insert(p_idx.clone(), id);
                Some(id)
            };
            let (ret_str, _) = &self.protos[*old_i];
            let shorty = shorty_for(
                ret_str,
                &self.protos[*old_i]
                    .1
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            );
            let shorty_idx = new_string_idx[&shorty];
            protos.push(ProtoRow {
                shorty_idx,
                return_type_idx: *ret_idx,
                parameters_list,
            });
            proto_remap_old_to_new[*old_i] = new_i as u32;
        }

        // 4. Sort fields by (class_idx, type_idx, name_idx). Each
        //    field key is in *new* type/string indices.
        let mut field_keys: Vec<((u32, u32, u32), usize)> = self
            .fields
            .iter()
            .enumerate()
            .map(|(old_i, (class, name, ty))| {
                let class_idx = new_type_idx[class];
                let type_idx = new_type_idx[ty];
                let name_idx = new_string_idx[name];
                ((class_idx, type_idx, name_idx), old_i)
            })
            .collect();
        field_keys.sort_by(|a, b| a.0.cmp(&b.0));
        let mut fields: Vec<FieldRow> = Vec::with_capacity(self.fields.len());
        let mut field_remap_old_to_new = vec![0u32; self.fields.len()];
        for (new_i, ((class_idx, type_idx, name_idx), old_i)) in field_keys.iter().enumerate() {
            fields.push(FieldRow {
                class_idx: *class_idx as u16,
                type_idx: *type_idx as u16,
                name_idx: *name_idx,
            });
            field_remap_old_to_new[*old_i] = new_i as u32;
        }

        // 5. Sort methods by (class_idx, name_idx, proto_idx) — the
        //    DEX spec actually sorts by (class, name, proto). The
        //    proto-id is the *new* (post-sort) index.
        let mut method_keys: Vec<((u32, u32, u32), usize)> = self
            .methods
            .iter()
            .enumerate()
            .map(|(old_i, (class, name, ret, params))| {
                let class_idx = new_type_idx[class];
                let name_idx = new_string_idx[name];
                // Find the new proto idx for this method's signature.
                let proto_old_idx = self.protos_dedup[&(ret.clone(), params.clone())];
                let proto_idx = proto_remap_old_to_new[proto_old_idx as usize];
                ((class_idx, name_idx, proto_idx), old_i)
            })
            .collect();
        method_keys.sort_by(|a, b| a.0.cmp(&b.0));
        let mut methods: Vec<MethodRow> = Vec::with_capacity(self.methods.len());
        let mut method_remap_old_to_new = vec![0u32; self.methods.len()];
        for (new_i, ((class_idx, name_idx, proto_idx), old_i)) in method_keys.iter().enumerate() {
            methods.push(MethodRow {
                class_idx: *class_idx as u16,
                proto_idx: *proto_idx as u16,
                name_idx: *name_idx,
            });
            method_remap_old_to_new[*old_i] = new_i as u32;
        }

        let final_idx = FinalIndices {
            strings,
            types,
            type_string_idx,
            protos,
            fields,
            methods,
        };
        let remap = Remap {
            string: string_remap_old_to_new,
            r#type: type_remap_old_to_new,
            proto: proto_remap_old_to_new,
            field: field_remap_old_to_new,
            method: method_remap_old_to_new,
        };
        (final_idx, remap, param_lists)
    }
}

/// Old-id → new-id remap tables produced by [`Pools::finalize`]. The
/// bytecode patcher uses these to rewrite the placeholder indices it
/// stored in instructions during the first pass.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct Remap {
    pub string: Vec<u32>,
    pub r#type: Vec<u32>,
    /// Currently unused — proto refs only appear inside method ids,
    /// which the writer remaps via `Pools::finalize` directly. Kept
    /// here for symmetry and so future PRs that emit `proto@BBBB`
    /// instructions (e.g. `invoke-polymorphic`) can use it.
    pub proto: Vec<u32>,
    pub field: Vec<u32>,
    pub method: Vec<u32>,
}

/// Compute the "shorty" descriptor for a method signature. A shorty
/// is a single character per type: `V`/`Z`/`B`/`S`/`C`/`I`/`J`/`F`/`D`
/// for primitives and `L` for any reference (including arrays).
///
/// The first character is the return type; the rest are parameters.
pub fn shorty_for(return_ty: &str, params: &[&str]) -> String {
    let mut s = String::new();
    s.push(shorty_char(return_ty));
    for p in params {
        s.push(shorty_char(p));
    }
    s
}

fn shorty_char(descriptor: &str) -> char {
    match descriptor.chars().next() {
        Some('V') => 'V',
        Some('Z') => 'Z',
        Some('B') => 'B',
        Some('S') => 'S',
        Some('C') => 'C',
        Some('I') => 'I',
        Some('J') => 'J',
        Some('F') => 'F',
        Some('D') => 'D',
        // L (object) and [ (array) both shorten to L.
        Some('L') | Some('[') => 'L',
        _ => 'L',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorty_basic_void() {
        assert_eq!(shorty_for("V", &[]), "V");
    }

    #[test]
    fn shorty_with_params() {
        assert_eq!(shorty_for("V", &["Ljava/lang/String;"]), "VL");
        assert_eq!(shorty_for("I", &["I", "I"]), "III");
    }

    #[test]
    fn shorty_array_param() {
        assert_eq!(shorty_for("V", &["[Ljava/lang/String;"]), "VL");
    }

    #[test]
    fn intern_strings_dedupe() {
        let mut p = Pools::new();
        let a = p.intern_string("hello");
        let b = p.intern_string("hello");
        let c = p.intern_string("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn finalize_sorts_strings() {
        let mut p = Pools::new();
        p.intern_string("zebra");
        p.intern_string("apple");
        p.intern_string("monkey");
        let (final_idx, remap, _params) = p.finalize();
        assert_eq!(final_idx.strings, vec!["apple", "monkey", "zebra"]);
        // The string we collected first ("zebra") should now have new
        // index 2.
        assert_eq!(remap.string[0], 2);
        assert_eq!(remap.string[1], 0);
        assert_eq!(remap.string[2], 1);
    }

    #[test]
    fn finalize_assigns_type_string_idx_correctly() {
        let mut p = Pools::new();
        p.intern_type("Ljava/lang/Object;");
        p.intern_type("LFoo;");
        let (final_idx, _, _) = p.finalize();
        // After sort: ["LFoo;", "Ljava/lang/Object;"].
        assert_eq!(final_idx.types, vec!["LFoo;", "Ljava/lang/Object;"]);
        // type_string_idx[i] gives the string-id of types[i]; the
        // strings are sorted lexicographically too, so "LFoo;" comes
        // before "Ljava/lang/Object;" in the string pool too.
        let lfoo_string = final_idx.strings.iter().position(|s| s == "LFoo;").unwrap();
        let lobj_string = final_idx
            .strings
            .iter()
            .position(|s| s == "Ljava/lang/Object;")
            .unwrap();
        assert_eq!(final_idx.type_string_idx[0], lfoo_string as u32);
        assert_eq!(final_idx.type_string_idx[1], lobj_string as u32);
    }
}
