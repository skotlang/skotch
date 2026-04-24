//! DEX (Dalvik Executable) bytecode emitter for skotch's MIR.
//!
//! Targets DEX format **version 035**, which is what every Android
//! version since API 13 reads. The writer is hand-rolled over
//! `byteorder`; we lift the same patterns the JVM backend uses.
//!
//! ## Architecture
//!
//! The DEX format is index-heavy: every method, field, type, and
//! string is referenced by an index into a sorted table. The string
//! table in particular **must be sorted in modified-UTF-8 byte
//! order**, and every other index table sorts by some derived key.
//! This makes index assignment a *post-collection* problem: we walk
//! the MIR collecting symbolic references first, then sort and assign
//! indices in a second pass.
//!
//! - [`pools`] holds the deferred-index pools (strings, types,
//!   protos, fields, methods, classes) and the sort+remap pass.
//! - [`bytecode`] turns MIR into DEX instruction bytes, using
//!   placeholder indices that the writer patches up after sort.
//! - [`writer`] lays out the file, computes section offsets, builds
//!   the map_list, writes the header, and patches the SHA-1 signature
//!   plus Adler32 checksum at the end.
//! - [`leb128`] is the canonical uleb128/sleb128 encoder used by the
//!   pools and the class data.
//!
//! ## What we currently cover
//!
//! - One class per `MirModule` (the wrapper class)
//! - `static main([Ljava/lang/String;)V` plus arbitrary other
//!   `static` top-level functions
//! - The `Println` intrinsic dispatched on argument type
//!   (String / Int)
//! - Integer arithmetic (add/sub/mul/div/rem)
//! - `invoke-static` between two top-level functions in the same
//!   wrapper class
//! - String literals via the string pool
//!
//! Branching, fields, instance methods, generics, and interfaces are
//! intentionally out of scope (they need additional fixtures to land
//! first).

mod bytecode;
mod leb128;
mod pools;
mod writer;

use skotch_mir::MirModule;

/// Compile a [`MirModule`] to a `.dex` file's bytes.
///
/// The result is a single `classes.dex` payload containing one
/// wrapper class with one method per top-level function in `module`.
pub fn compile_module(module: &MirModule) -> Vec<u8> {
    writer::write_dex(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_intern::Interner;
    use skotch_lexer::lex;
    use skotch_mir_lower::lower_file;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;
    use skotch_typeck::type_check;

    fn compile(src: &str) -> Vec<u8> {
        let mut interner = Interner::new();
        let mut diags = skotch_diagnostics::Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags, None);
        let t = type_check(&ast, &r, &mut interner, &mut diags, None);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "InputKt", None);
        assert!(!diags.has_errors(), "diagnostics: {:?}", diags);
        compile_module(&m)
    }

    #[test]
    fn emit_dex_starts_with_magic() {
        let bytes = compile(r#"fun main() { println("Hello, world!") }"#);
        assert_eq!(&bytes[0..8], b"dex\n035\0");
    }

    #[test]
    fn emit_dex_contains_hello_string() {
        let bytes = compile(r#"fun main() { println("Hello, world!") }"#);
        assert!(
            bytes.windows(13).any(|w| w == b"Hello, world!"),
            "expected `Hello, world!` somewhere in the DEX bytes"
        );
    }

    #[test]
    fn emit_dex_contains_class_descriptor() {
        let bytes = compile(r#"fun main() { println("Hi") }"#);
        // The descriptor "LInputKt;" is encoded as a MUTF-8 string
        // somewhere in the data section.
        assert!(bytes.windows(9).any(|w| w == b"LInputKt;"));
    }

    #[test]
    fn emit_dex_contains_println_method_name() {
        let bytes = compile(r#"fun main() { println("Hi") }"#);
        assert!(bytes.windows(7).any(|w| w == b"println"));
    }

    #[test]
    fn emit_dex_arithmetic_compiles() {
        let bytes = compile("fun main() { println(1 + 2 * 3) }");
        assert_eq!(&bytes[0..8], b"dex\n035\0");
        // Adler32 checksum is computed over header[12..]; it being
        // all-zero after writing would indicate the patch step ran but
        // produced zeros, which is statistically impossible for a real
        // DEX file. Verify it's non-zero.
        assert_ne!(&bytes[8..12], &[0u8; 4]);
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: emit_dex_with_two_methods (fixture 08)
    // TODO: emit_dex_with_local_var   (fixture 04)
    // TODO: emit_dex_passes_dexdump   (gated on dexdump availability)
    // TODO: emit_dex_byte_equal_to_committed_golden  (regression net)
}
