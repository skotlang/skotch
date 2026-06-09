//! Source Information Layer — Kotlin's lossless concrete syntax tree
//! plus the YAML serialization the SIL CLI and golden tests use.
//!
//! Three things this crate buys you:
//!
//! 1. **`parse_sil`** — runs the trivia-preserving lexer + the SIL
//!    grammar + a [`SilSink`] and returns a [`SilTree`]. The tree's
//!    leaves concatenate back to the original source.
//! 2. **`emit_yaml` / `parse_yaml`** — round-trip a [`SilTree`]
//!    through the YAML format used by `scripts/kotlin-psi.main.kts`.
//!    Byte-identical to that script's output for the same source.
//! 3. **`reconstruct`** — walk a [`SilTree`] and produce the original
//!    `.kt` text. Combined with `parse_yaml` this means YAML → text
//!    works.
//!
//! Architecturally the crate sits next to the FIR pipeline rather
//! than under it: SIL parsing never runs during a Skotch compile.
//! The two share `skotch-syntax::SyntaxKind`, `skotch-lexer`, and
//! `skotch-parser-core`, but the SIL grammar lives here in
//! [`mod grammar`] and the FIR grammar stays in `skotch-parser`.

mod grammar;
mod kdoc;
mod sink;
mod tree;
mod yaml_emit;
mod yaml_kind;
mod yaml_parse;

pub use sink::SilSink;
pub use tree::{SilData, SilNode, SilTree};
pub use yaml_emit::emit_yaml;
pub use yaml_kind::{syntax_kind_from_name, syntax_kind_name};
pub use yaml_parse::{parse_yaml, YamlParseError};

use skotch_diagnostics::Diagnostics;
use skotch_lexer::{lex_with, LexerOptions};
use skotch_parser_core::{Input, ParseOutput, Parser as PcParser};
use skotch_span::FileId;

/// Parse `source` into a [`SilTree`].
///
/// `file_path` is written verbatim into the tree's `file:` header so
/// downstream YAML diffs can include path context. CRLF/CR sequences
/// in the source are normalized to LF before lexing — the YAML's
/// `crlf_normalized` field records whether the original had any.
pub fn parse_sil(file_path: impl Into<String>, source: &str) -> SilTree {
    let mut diags = Diagnostics::new();
    parse_sil_with_diagnostics(file_path, source, &mut diags)
}

/// Same as [`parse_sil`] but appends parse errors to `diags`. Use this
/// when you need to surface lexer-level diagnostics — the SIL tree is
/// still produced regardless.
pub fn parse_sil_with_diagnostics(
    file_path: impl Into<String>,
    source: &str,
    diags: &mut Diagnostics,
) -> SilTree {
    let file_path = file_path.into();
    let original = source;
    let normalized = if source.contains('\r') {
        source.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        source.to_string()
    };
    let crlf_normalized = normalized.len() != original.len();
    let file_id = FileId(0);

    let lexed = lex_with(
        file_id,
        &normalized,
        diags,
        LexerOptions {
            preserve_trivia: true,
        },
    );
    let input = Input::new(&lexed, &normalized);
    let mut parser = PcParser::new(&input);
    grammar::parse_file_root(&mut parser);
    let (events, errors) = parser.finish();

    let mut sink = SilSink::new(file_path, normalized.len() as u32, crlf_normalized, file_id);
    ParseOutput::new(events, errors).process(&input, &mut sink);
    sink.finish()
}

/// Parse the YAML representation back into a tree, then reconstruct
/// the source bytes by concatenating every leaf's `text` in
/// pre-order. The full roundtrip:
///
/// ```text
/// source.kt
///   └─ parse_sil ─┐
///                 ▼
///              SilTree ── emit_yaml ──▶ yaml string
///                              │
///                              ▼
///                          parse_yaml
///                              │
///                              ▼
///                           SilTree
///                              │
///                              ▼
///                          reconstruct ──▶ source.kt
/// ```
///
/// Returns the reconstructed source.
pub fn reconstruct_from_yaml(yaml: &str) -> Result<String, YamlParseError> {
    Ok(parse_yaml(yaml)?.reconstruct())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_then_reconstruct_minimal_source() {
        let src = "package foo\n";
        let tree = parse_sil("test.kt", src);
        assert_eq!(tree.reconstruct(), src);
    }

    #[test]
    fn parse_then_reconstruct_preserves_comments() {
        let src = "// hi\npackage foo /* trail */\n";
        let tree = parse_sil("test.kt", src);
        assert_eq!(tree.reconstruct(), src);
    }

    #[test]
    fn yaml_roundtrip_through_full_pipeline() {
        let src = "package foo\n\nimport bar.baz\n\nfun main() {\n    println(\"hi\")\n}\n";
        let tree = parse_sil("test.kt", src);
        assert_eq!(tree.reconstruct(), src, "first reconstruct lost bytes");
        let yaml = emit_yaml(&tree);
        let back = reconstruct_from_yaml(&yaml).expect("yaml -> source");
        assert_eq!(back, src, "yaml roundtrip lost bytes");
    }

    #[test]
    fn yaml_is_idempotent() {
        let src = "package x\n";
        let tree = parse_sil("a.kt", src);
        let y1 = emit_yaml(&tree);
        let tree2 = parse_yaml(&y1).expect("re-parse");
        let y2 = emit_yaml(&tree2);
        assert_eq!(y1, y2);
    }

    #[test]
    fn parse_then_reconstruct_data_class_with_kdoc() {
        let src = r#"package demo

/**
 * KDoc.
 * @param x stuff
 */
data class Box<T>(val value: T) {
    fun map(f: (T) -> String): String = f(value)
}
"#;
        let tree = parse_sil("demo.kt", src);
        assert_eq!(
            tree.reconstruct(),
            src,
            "reconstruct mismatch — leaves are lossy"
        );
    }
}
