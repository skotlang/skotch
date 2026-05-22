//! MIR module merging for multi-file compilation.
//!
//! When a project has multiple `.kt` source files, each is compiled to
//! its own [`MirModule`]. Before backend codegen, they must be merged
//! into a single module. This requires remapping [`StringId`]s (and
//! later [`FuncId`]s for cross-file calls).

use skotch_mir::{MirConst, MirModule, Rvalue, Stmt, StringId};

/// Merge `other` into `into`, remapping string IDs so the combined
/// string pool is consistent. If `into` has no wrapper class set,
/// copies `other`'s wrapper class.
pub fn merge_modules(into: &mut MirModule, other: MirModule) {
    if into.wrapper_class.is_empty() {
        into.wrapper_class = other.wrapper_class.clone();
    }

    // Build a remap table: other's StringId → into's StringId.
    let mut remap: Vec<u32> = Vec::with_capacity(other.strings.len());
    for s in &other.strings {
        let new_id = into.intern_string(s);
        remap.push(new_id.0);
    }

    // Remap and merge functions.
    for mut f in other.functions {
        for block in &mut f.blocks {
            for stmt in &mut block.stmts {
                let Stmt::Assign { value, .. } = stmt;
                if let Rvalue::Const(MirConst::String(sid)) = value {
                    if (sid.0 as usize) < remap.len() {
                        *sid = StringId(remap[sid.0 as usize]);
                    }
                }
            }
        }
        into.functions.push(f);
    }

    // Merge classes (with string remapping). When the same class name
    // appears in both modules (e.g. one as a real declaration and one
    // as a cross-file stub), keep the real one and drop the stub —
    // field types resolved from constructor params on the home file
    // win over the erased shapes recovered from a `PackageSymbolTable`.
    for mut cls in other.classes {
        if let Some(existing) = into.classes.iter().position(|c| c.name == cls.name) {
            let existing_is_stub = into.classes[existing].is_cross_file_stub;
            let new_is_stub = cls.is_cross_file_stub;
            match (existing_is_stub, new_is_stub) {
                (true, false) => {
                    into.classes.remove(existing);
                }
                (false, _) => {
                    continue;
                }
                (true, true) => {
                    continue;
                }
            }
        }
        // Remap strings in constructor and methods.
        for f in std::iter::once(&mut cls.constructor)
            .chain(cls.methods.iter_mut())
            .chain(cls.secondary_constructors.iter_mut())
        {
            for block in &mut f.blocks {
                for stmt in &mut block.stmts {
                    let Stmt::Assign { value, .. } = stmt;
                    if let Rvalue::Const(MirConst::String(sid)) = value {
                        if (sid.0 as usize) < remap.len() {
                            *sid = StringId(remap[sid.0 as usize]);
                        }
                    }
                }
            }
        }
        into.classes.push(cls);
    }

    // Merge enum names.
    into.enum_names.extend(other.enum_names);
}
