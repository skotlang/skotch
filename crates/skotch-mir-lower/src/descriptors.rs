//! JVM method-descriptor inspection helpers used by mir-lower.
//!
//! These helpers parse short fragments of JVM method-descriptor syntax
//! (`(I[Lkotlin/Pair;)Lkotlin/Unit;`) without constructing a full AST.
//! Kept narrow on purpose — they're hot-paths in the composable +
//! call-site padding code, and the regex/parser combos in other Kotlin
//! tooling would be overkill.
//!
//! `#[allow(dead_code)]` on the whole module: all the helpers were
//! consumed by the legacy `lib.rs` that was deleted in the typed-AST
//! cutover. They're retained here because the typed mir-lower is
//! expected to reach call-site shapes that need them as it grows.
#![allow(dead_code)]

use skotch_types::Ty;

/// Parse the return type out of a JVM method descriptor like
/// `(Landroid/os/Bundle;)V` → `Ty::Unit` or `(I)Z` → `Ty::Bool`.
/// Reference types collapse to `Ty::Any` (callers that need the
/// concrete class can re-parse) — this helper is for picking up
/// Composer-less call-site return types from classinfo.
pub(crate) fn ty_from_descriptor_return(desc: &str) -> Ty {
    let ret = desc.rsplit(')').next().unwrap_or("V");
    let mut chars = ret.chars();
    match chars.next() {
        Some('V') => Ty::Unit,
        Some('Z') => Ty::Bool,
        Some('B') => Ty::Byte,
        Some('S') => Ty::Short,
        Some('C') => Ty::Char,
        Some('I') => Ty::Int,
        Some('J') => Ty::Long,
        Some('F') => Ty::Float,
        Some('D') => Ty::Double,
        Some('L') => {
            // L<name>;
            let inner: String = chars.take_while(|&c| c != ';').collect();
            if inner.is_empty() {
                Ty::Any
            } else {
                Ty::Class(inner)
            }
        }
        Some('[') => Ty::Any, // arrays — caller can refine
        _ => Ty::Any,
    }
}

/// Parse a JVM method descriptor and return the 0-based index of the
/// `Landroidx/compose/runtime/Composer;` parameter. Used to fill in the
/// `$composer` slot at the correct position when patching composable
/// call sites — the simple "second-from-last" heuristic places the
/// Composer wrong when the descriptor ends with a trailing `$default`
/// mask (e.g. `(DrawerValue, Function1, Composer, I, I)`).
pub(crate) fn composer_position_in_descriptor(desc: &str) -> Option<usize> {
    let inside = desc.strip_prefix('(').and_then(|s| s.split_once(')'))?.0;
    let mut idx = 0usize;
    let mut chars = inside.chars();
    while let Some(c) = chars.next() {
        match c {
            'L' => {
                let mut name = String::new();
                for cc in chars.by_ref() {
                    if cc == ';' {
                        break;
                    }
                    name.push(cc);
                }
                if name == "androidx/compose/runtime/Composer" {
                    return Some(idx);
                }
                idx += 1;
            }
            '[' => continue,
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' => idx += 1,
            _ => continue,
        }
    }
    None
}

/// If the last param of a JVM method descriptor is an object array
/// (`[Lsome/Class;`), return the JVM internal name of the element class
/// (e.g. `kotlin/Pair`). Used to detect vararg slots so individually-
/// passed args at the call site can be packed into a fresh array.
pub(crate) fn vararg_element_class(desc: &str) -> Option<String> {
    let inside = desc.strip_prefix('(').and_then(|s| s.split_once(')'))?.0;
    // Walk to find the LAST param.
    let mut last_kind: Option<String> = None;
    let mut chars = inside.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'L' => {
                let mut name = String::from("L");
                for cc in chars.by_ref() {
                    name.push(cc);
                    if cc == ';' {
                        break;
                    }
                }
                last_kind = Some(name);
            }
            '[' => {
                let mut s = String::from("[");
                while let Some(&nc) = chars.peek() {
                    if nc == '[' {
                        s.push('[');
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(&nc) = chars.peek() {
                    if nc == 'L' {
                        chars.next();
                        s.push('L');
                        for cc in chars.by_ref() {
                            s.push(cc);
                            if cc == ';' {
                                break;
                            }
                        }
                    } else {
                        s.push(chars.next().unwrap());
                    }
                }
                last_kind = Some(s);
            }
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' => {
                last_kind = Some(c.to_string());
            }
            _ => continue,
        }
    }
    let last = last_kind?;
    // Need exactly `[L<name>;` shape — single-dim object array.
    let stripped = last.strip_prefix("[L")?.strip_suffix(';')?;
    if stripped.contains('[') {
        return None;
    }
    Some(stripped.to_string())
}

/// Parse a JVM method descriptor and return the sequence of param Ty values,
/// one per param slot. Used to pad composable call args with correctly
/// typed placeholders (e.g. Int(0) for `$changed`/`$default`, not null).
pub(crate) fn param_tys_from_descriptor(desc: &str) -> Vec<Ty> {
    let inside = match desc.strip_prefix('(').and_then(|s| s.split_once(')')) {
        Some((i, _)) => i,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut chars = inside.chars();
    while let Some(c) = chars.next() {
        match c {
            'L' => {
                let mut name = String::new();
                for cc in chars.by_ref() {
                    if cc == ';' {
                        break;
                    }
                    name.push(cc);
                }
                out.push(if name == "java/lang/String" {
                    Ty::String
                } else {
                    Ty::Class(name)
                });
            }
            '[' => {
                let mut depth = 1usize;
                while let Some(cc) = chars.next() {
                    if cc == '[' {
                        depth += 1;
                    } else if cc == 'L' {
                        for ccc in chars.by_ref() {
                            if ccc == ';' {
                                break;
                            }
                        }
                        break;
                    } else {
                        break;
                    }
                }
                let _ = depth;
                out.push(Ty::Any);
            }
            'Z' => out.push(Ty::Bool),
            'B' => out.push(Ty::Byte),
            'S' => out.push(Ty::Short),
            'C' => out.push(Ty::Char),
            'I' => out.push(Ty::Int),
            'J' => out.push(Ty::Long),
            'F' => out.push(Ty::Float),
            'D' => out.push(Ty::Double),
            _ => continue,
        }
    }
    out
}

/// Look up the best method descriptor for a composable function call.
/// Prefers overloads whose descriptor contains "Composer" (composable),
/// trying various param counts to account for default parameters.
pub(crate) fn lookup_composable_descriptor(
    class_path: &str,
    method_name: &str,
    user_arg_count: usize,
) -> Option<String> {
    // Try a range of param counts: the composable overload has at least
    // user_args + 2 ($composer + $changed), potentially more with
    // default params ($default bitmask) and the default params themselves.
    for extra in &[2, 3, 4, 5, 6, 7, 8] {
        if let Some(d) = skotch_classinfo::lookup_method_descriptor(
            class_path,
            method_name,
            user_arg_count + extra,
        ) {
            if d.contains("Composer") {
                return Some(d);
            }
        }
    }
    // No composable overload found — try exact match first.
    if let Some(d) =
        skotch_classinfo::lookup_method_descriptor(class_path, method_name, user_arg_count)
    {
        return Some(d);
    }
    // Then try non-composable overloads with extra params — this catches
    // Kotlin defaults like `mutableStateOf(value, policy = null)` whose
    // descriptor has user_args + 1 params but the user only provided
    // the value. The caller's per-slot padding will fill missing
    // reference args with null and primitives with 0 based on the
    // descriptor's character at each slot.
    for extra in &[1usize, 2, 3, 4] {
        if let Some(d) = skotch_classinfo::lookup_method_descriptor(
            class_path,
            method_name,
            user_arg_count + extra,
        ) {
            return Some(d);
        }
    }
    None
}

/// Parse a JVM method descriptor into a vec of per-slot "kind" characters:
/// 'I' for int-shaped primitives, 'J' for long, 'F' for float, 'D' for double,
/// 'L' for reference types (including arrays). Returns one entry per
/// parameter slot. Used by call-site arg-padding to pick the right default
/// value (0 for primitives, null for refs).
pub(crate) fn parse_descriptor_param_chars(desc: &str) -> Vec<char> {
    let inner = desc
        .strip_prefix('(')
        .and_then(|s| s.split(')').next())
        .unwrap_or("");
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'B' | 'C' | 'I' | 'S' | 'Z' => out.push('I'),
            'J' => out.push('J'),
            'F' => out.push('F'),
            'D' => out.push('D'),
            'L' => {
                for sc in chars.by_ref() {
                    if sc == ';' {
                        break;
                    }
                }
                out.push('L');
            }
            '[' => {
                // Array: skip the element descriptor entirely (including any nested L...;)
                while let Some(&sc) = chars.peek() {
                    if sc == '[' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(elem) = chars.next() {
                    if elem == 'L' {
                        for sc in chars.by_ref() {
                            if sc == ';' {
                                break;
                            }
                        }
                    }
                }
                out.push('L');
            }
            _ => {}
        }
    }
    out
}
