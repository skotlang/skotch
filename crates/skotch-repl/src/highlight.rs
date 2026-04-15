//! Syntax highlighting for Kotlin source text.
//!
//! [`SkotchHighlighter`] implements reedline's [`Highlighter`] trait by
//! running the real skotch lexer on every keystroke and mapping token
//! kinds to ANSI colours. Because it delegates to [`skotch_lexer::lex`],
//! it always agrees with the compiler on what constitutes a keyword, a
//! string literal, a number, etc.
//!
//! ## Colour scheme
//!
//! | Token class              | Colour         |
//! |--------------------------|----------------|
//! | Keywords                 | Yellow         |
//! | `true` / `false` / `null`| Green (bold)   |
//! | String literals          | Green          |
//! | Numeric literals         | Green          |
//! | Identifiers              | Blue           |
//! | Operators / punctuation  | Default        |
//! | Errors / incomplete      | Red            |
//!
//! ## Reuse
//!
//! The core logic lives in [`highlight_kotlin`], a free function that
//! returns `Vec<(Style, String)>`. It can be used outside of reedline
//! — for example in an LSP semantic-tokens provider or a static HTML
//! renderer — without pulling in the reedline dependency.

use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};

use skotch_diagnostics::Diagnostics;
use skotch_span::FileId;
use skotch_syntax::TokenKind;

// ── Colour palette ──────────────────────────────────────────────────

/// Keyword colour (e.g. `fun`, `val`, `class`, `if`, `return`).
const KW_STYLE: fn() -> Style = || Style::new().fg(Color::Yellow);

/// Literal value colour (strings, numbers, `true`, `false`, `null`).
const LIT_STYLE: fn() -> Style = || Style::new().fg(Color::Green);

/// Bold literal colour for boolean/null keywords.
const LIT_KW_STYLE: fn() -> Style = || Style::new().fg(Color::Green).bold();

/// Identifier colour (variable names, function names, types).
const IDENT_STYLE: fn() -> Style = || Style::new().fg(Color::Blue);

/// Default text colour (operators, punctuation, unknown).
const DEFAULT_STYLE: fn() -> Style = Style::new;

/// Error / incomplete token colour.
const ERROR_STYLE: fn() -> Style = || Style::new().fg(Color::Red);

// ── Public reusable API ─────────────────────────────────────────────

/// Lex `source` as Kotlin and return styled segments.
///
/// This is the reusable core — it has no reedline dependency (only
/// `nu_ansi_term`). Call it from any context that needs coloured Kotlin
/// source.
pub fn highlight_kotlin(source: &str) -> Vec<(Style, String)> {
    let mut diags = Diagnostics::new();
    let file_id = FileId(u32::MAX); // scratch id — not persisted
    let lexed = skotch_lexer::lex(file_id, source, &mut diags);

    let bytes = source.as_bytes();
    let mut segments: Vec<(Style, String)> = Vec::new();
    let mut cursor: usize = 0; // byte offset into `source`

    for tok in &lexed.tokens {
        let start = tok.span.start as usize;
        let end = tok.span.end as usize;

        // Safety-clamp to source length (the lexer should never
        // produce out-of-range spans, but be defensive).
        let start = start.min(bytes.len());
        let end = end.min(bytes.len());

        // Emit any gap between the previous token's end and this
        // token's start as unstyled text (whitespace, comments).
        if start > cursor {
            segments.push((DEFAULT_STYLE(), source[cursor..start].to_string()));
        }

        if start >= end {
            // Zero-width token (e.g. Eof). Skip.
            cursor = end;
            continue;
        }

        // Skip Error tokens — in a live REPL the user is mid-typing,
        // so incomplete input (unterminated strings, etc.) is normal.
        // The source bytes are still emitted by the gap/trailing-text
        // fallback in default style, avoiding distracting red flashes.
        if tok.kind == TokenKind::Error {
            continue;
        }

        let text = &source[start..end];
        let style = style_for_kind(tok.kind);
        segments.push((style, text.to_string()));
        cursor = end;
    }

    // Trailing text after the last token (rare — usually just
    // whitespace the lexer didn't emit a token for).
    if cursor < bytes.len() {
        segments.push((DEFAULT_STYLE(), source[cursor..].to_string()));
    }

    segments
}

// ── Reedline integration ────────────────────────────────────────────

/// Syntax highlighter for the skotch REPL.
///
/// Wraps [`highlight_kotlin`] behind reedline's [`Highlighter`] trait
/// so each keystroke re-lexes the input buffer and applies colours.
pub(crate) struct SkotchHighlighter;

impl Highlighter for SkotchHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        for segment in highlight_kotlin(line) {
            styled.push(segment);
        }
        styled
    }
}

// ── Token → style mapping ───────────────────────────────────────────

fn style_for_kind(kind: TokenKind) -> Style {
    use TokenKind::*;
    match kind {
        // Keywords.
        KwFun | KwVal | KwVar | KwIf | KwElse | KwReturn | KwWhile | KwDo | KwWhen | KwFor
        | KwIn | KwBreak | KwContinue | KwClass | KwObject | KwPackage | KwImport | KwConst
        | KwThrow | KwTry | KwCatch | KwFinally | KwIs | KwAs | KwSuper | KwInit | KwData
        | KwEnum | KwInterface | KwSealed | KwOverride | KwOpen | KwAbstract | KwInfix
        | KwInline | KwPrivate | KwProtected | KwInternal | KwOperator | KwVararg
        | KwConstructor | KwLateinit => KW_STYLE(),

        // Boolean / null literals — keyword-like but coloured as literals.
        KwTrue | KwFalse | KwNull => LIT_KW_STYLE(),

        // Numeric literals.
        IntLit | LongLit | DoubleLit => LIT_STYLE(),

        // String literals and template pieces.
        StringLit | StringStart | StringChunk | StringEnd => LIT_STYLE(),
        StringIdentRef | StringExprStart | StringExprEnd => LIT_STYLE(),

        // Identifiers.
        Ident => IDENT_STYLE(),

        // Errors.
        Error => ERROR_STYLE(),

        // Everything else: punctuation, operators, newlines, Eof.
        _ => DEFAULT_STYLE(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that highlighting produces segments whose text concatenation
    /// equals the original source (no characters lost or added).
    #[test]
    fn roundtrip_preserves_text() {
        let cases = [
            "val x = 42",
            r#"println("hello $name")"#,
            "fun greet() = println(\"hi\")",
            "if (true) 1 else 2",
            "java.net.URI(\"https://skotch.dev\")",
            "",
            "   ",
        ];
        for src in &cases {
            let segments = highlight_kotlin(src);
            let reconstructed: String = segments.iter().map(|(_, s)| s.as_str()).collect();
            assert_eq!(&reconstructed, src, "roundtrip mismatch for: {src:?}");
        }
    }

    /// Keywords should get the keyword style.
    #[test]
    fn keywords_are_yellow() {
        let segments = highlight_kotlin("val x = 42");
        // First non-whitespace segment should be "val" with yellow.
        let (style, text) = &segments[0];
        assert_eq!(text, "val");
        assert_eq!(style.foreground, Some(Color::Yellow));
    }

    /// Numeric literals should be green.
    #[test]
    fn numbers_are_green() {
        let segments = highlight_kotlin("val x = 42");
        let num_seg = segments.iter().find(|(_, t)| t == "42").unwrap();
        assert_eq!(num_seg.0.foreground, Some(Color::Green));
    }

    /// Identifiers should be blue.
    #[test]
    fn idents_are_blue() {
        let segments = highlight_kotlin("val x = 42");
        let id_seg = segments.iter().find(|(_, t)| t == "x").unwrap();
        assert_eq!(id_seg.0.foreground, Some(Color::Blue));
    }

    /// Boolean literals should be bold green.
    #[test]
    fn booleans_are_bold_green() {
        let segments = highlight_kotlin("true");
        let (style, text) = &segments[0];
        assert_eq!(text, "true");
        assert_eq!(style.foreground, Some(Color::Green));
        assert!(style.is_bold);
    }

    /// String literals should be green.
    #[test]
    fn strings_are_green() {
        let segments = highlight_kotlin(r#""hello""#);
        // All segments of the string should be green.
        for (style, _) in &segments {
            assert_eq!(style.foreground, Some(Color::Green));
        }
    }
}
