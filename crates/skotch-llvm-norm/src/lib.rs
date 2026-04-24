//! Normalize LLVM textual IR into a stable form for golden diffing.
//!
//! Two independent compilers (skotch and `kotlinc-native` + `clang -S
//! -emit-llvm`) will produce wildly different LLVM IR for the same
//! Kotlin source — kotlinc-native bundles its entire runtime, emits
//! ObjC bridge stubs, and includes target-specific datalayout/triple.
//! For golden-file diffing to be useful at all we strip the
//! host-specific noise and reduce both files to a stable text form
//! that captures *what skotch itself emits*. This isn't a faithful
//! comparison against kotlinc-native — that's intentionally out of
//! scope, just like the JVM normalizer doesn't try to match every
//! `kotlin.Metadata` annotation.
//!
//! ## What we strip
//!
//! - `target triple = ...`
//! - `target datalayout = ...`
//! - `attributes #N = { ... }` blocks
//! - `!llvm.module.flags = ...` and similar `!metadata` lines
//! - Lines starting with `!` (debug info, named metadata)
//! - `source_filename = ...`
//! - Blank-line runs collapse to a single blank line
//!
//! ## What we keep
//!
//! - All `define`s and `declare`s, sorted by name
//! - All `@global` constants, sorted by name
//! - The function bodies themselves (we *do not* alpha-rename SSA
//!   registers — they're already deterministic in skotch's
//!   output, and the diffs are easier to read with the original
//!   names)
//!
//! ## Out of scope
//!
//! - Cross-compiler equivalence with kotlinc-native (its IR is at
//!   least 100x larger and references stdlib internals we don't
//!   reproduce)
//! - Comparing optimized vs unoptimized output
//! - Stripping `dbg !N` annotations from instruction lines (skotch
//!   doesn't emit them)

/// Normalize an LLVM IR text blob.
pub fn normalize(text: &str) -> String {
    let mut globals: Vec<String> = Vec::new();
    let mut definitions: Vec<(String, String)> = Vec::new();
    let mut declarations: Vec<String> = Vec::new();

    let mut current_def: Option<(String, String)> = None;
    let mut in_attributes = false;

    for raw in text.lines() {
        let line = raw.trim_end();
        // Strip the noisy header lines.
        if line.starts_with("target ") {
            continue;
        }
        if line.starts_with("source_filename") {
            continue;
        }
        if line.starts_with("; ModuleID") {
            continue;
        }
        if line.starts_with("attributes #") {
            in_attributes = true;
            continue;
        }
        if in_attributes {
            // attribute groups are single-line in LLVM 19+, but be
            // defensive: anything until the next blank line or `define`
            // is part of the same group.
            if line.is_empty() || line.starts_with("define ") || line.starts_with("declare ") {
                in_attributes = false;
                // fall through
            } else {
                continue;
            }
        }
        if line.starts_with('!') || line.starts_with("!llvm") {
            continue;
        }

        if let Some((_, body)) = current_def.as_mut() {
            // We're inside a function body. Append until we see `}`.
            body.push_str(line);
            body.push('\n');
            if line.starts_with('}') {
                let (name, body) = current_def.take().unwrap();
                definitions.push((name, body));
            }
            continue;
        }

        if line.starts_with("define ") {
            // Extract the function name from `define <ret> @name(...) {`
            let name = extract_function_name(line).unwrap_or_else(|| "<?>".to_string());
            let mut body = String::new();
            body.push_str(line);
            body.push('\n');
            current_def = Some((name, body));
            continue;
        }

        if line.starts_with("declare ") {
            declarations.push(line.to_string());
            continue;
        }

        if line.starts_with('@') {
            globals.push(line.to_string());
            continue;
        }

        // Blank lines and anything else: drop. We don't preserve
        // blank lines between sections in normalized output.
    }

    // Stable ordering.
    globals.sort();
    declarations.sort();
    definitions.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    out.push_str("; --- globals ---\n");
    for g in globals {
        out.push_str(&g);
        out.push('\n');
    }
    out.push_str("\n; --- declarations ---\n");
    for d in declarations {
        out.push_str(&d);
        out.push('\n');
    }
    out.push_str("\n; --- definitions ---\n");
    for (name, body) in definitions {
        out.push_str(&format!("; @{name}\n"));
        out.push_str(&body);
        out.push('\n');
    }
    out
}

fn extract_function_name(define_line: &str) -> Option<String> {
    // `define <attrs> <ret> @name(<args>) <attrs> {`
    let at = define_line.find('@')?;
    let rest = &define_line[at + 1..];
    let end = rest.find('(')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_target_triple_and_datalayout() {
        let input = r#"
target triple = "arm64-apple-macosx14.0.0"
target datalayout = "e-m:o"
source_filename = "Hello"
@.str.0 = private constant [4 x i8] c"hi\00"
declare i32 @puts(ptr)
define i32 @main() {
entry:
  call i32 @puts(ptr @.str.0)
  ret i32 0
}
"#;
        let n = normalize(input);
        assert!(!n.contains("target triple"));
        assert!(!n.contains("target datalayout"));
        assert!(!n.contains("source_filename"));
        assert!(n.contains("@.str.0 = private"));
        assert!(n.contains("declare i32 @puts(ptr)"));
        assert!(n.contains("@main"));
    }

    #[test]
    fn sorts_globals_and_definitions() {
        let input = r#"
@b = private constant [1 x i8] c"\00"
@a = private constant [1 x i8] c"\00"
define void @zebra() {
  ret void
}
define void @apple() {
  ret void
}
"#;
        let n = normalize(input);
        let a_pos = n.find("@a =").unwrap();
        let b_pos = n.find("@b =").unwrap();
        let apple_pos = n.find("; @apple").unwrap();
        let zebra_pos = n.find("; @zebra").unwrap();
        assert!(a_pos < b_pos);
        assert!(apple_pos < zebra_pos);
    }

    #[test]
    fn drops_attribute_groups_and_metadata() {
        let input = r#"
@x = private constant [1 x i8] c"\00"
attributes #0 = { nounwind }
!llvm.module.flags = !{!0}
!0 = !{i32 1, !"wchar_size", i32 4}
"#;
        let n = normalize(input);
        assert!(!n.contains("attributes #"));
        assert!(!n.contains("!llvm"));
        assert!(!n.contains("wchar_size"));
        assert!(n.contains("@x = private"));
    }
}
