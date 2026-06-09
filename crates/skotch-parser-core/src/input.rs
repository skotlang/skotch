//! Token-source adapter the parser reads from.
//!
//! Wraps a [`LexedFile`] so the parser can index by logical token
//! position (skipping trivia that the FIR path didn't lex, or
//! preserving it when the SIL path did). For now `Input` is a thin
//! borrow; if profiling shows it's hot we can swap to a `&[Token]`
//! direct slice in the parser.

use skotch_lexer::LexedFile;
use skotch_span::Span;
use skotch_syntax::{SyntaxKind, Token, TokenKind};

/// Read-only view over the lexed token stream the parser is consuming.
///
/// Holds a borrow of the [`LexedFile`] plus a slice of the original
/// source text (needed so `text_of` can return the verbatim bytes
/// behind any token — what the `TreeSink::token` call needs).
pub struct Input<'a> {
    lexed: &'a LexedFile,
    source: &'a str,
}

impl<'a> Input<'a> {
    pub fn new(lexed: &'a LexedFile, source: &'a str) -> Self {
        Self { lexed, source }
    }

    /// Total token count, including the trailing `Eof`.
    pub fn len(&self) -> usize {
        self.lexed.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lexed.tokens.is_empty()
    }

    /// Returns the token kind at `pos` as a [`SyntaxKind`], or
    /// [`SyntaxKind::EOF`] when past the end.
    pub fn kind(&self, pos: usize) -> SyntaxKind {
        match self.lexed.tokens.get(pos) {
            Some(t) => SyntaxKind::from_token_kind(t.kind),
            None => SyntaxKind::EOF,
        }
    }

    /// Returns the raw [`TokenKind`] at `pos`. Useful for FIR-path
    /// callers that match against the existing TokenKind enum.
    pub fn token_kind(&self, pos: usize) -> TokenKind {
        match self.lexed.tokens.get(pos) {
            Some(t) => t.kind,
            None => TokenKind::Eof,
        }
    }

    /// Returns the [`Token`] at `pos`, or the trailing `Eof` token if
    /// past the end (so callers never need to bounds-check).
    pub fn token(&self, pos: usize) -> Token {
        self.lexed.tokens.get(pos).copied().unwrap_or(Token {
            kind: TokenKind::Eof,
            span: Span {
                file: self.lexed.file,
                start: self.source.len() as u32,
                end: self.source.len() as u32,
            },
        })
    }

    /// The verbatim source text for the token at `pos`, or "" past EOF.
    pub fn text_of(&self, pos: usize) -> &'a str {
        let t = self.token(pos);
        let start = t.span.start as usize;
        let end = (t.span.end as usize).min(self.source.len());
        &self.source[start..end]
    }

    /// Borrow the underlying source text.
    pub fn source(&self) -> &'a str {
        self.source
    }
}
