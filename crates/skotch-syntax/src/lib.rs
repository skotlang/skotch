//! Token kinds + lexical / syntactic enums shared across the front-end.
//!
//! The legacy Box-tree AST (`pub mod ast`) was removed as part of the
//! typed-AST cutover. What's left:
//!
//! - [`syntax_kind::SyntaxKind`] — every node-kind tag the lexer /
//!   parser / SIL grammar / typed AST agree on.
//! - [`token`] — `Token`, `TokenKind`, payload variants.
//! - [`Visibility`] — `public` / `private` / `protected` / `internal`,
//!   used by the typed AST's accessors.
//!
//! Consumers of the typed AST should import [`skotch_ast`]; consumers
//! of the SIL tree should import [`skotch_sil`].

pub mod syntax_kind;
pub mod token;

pub use syntax_kind::SyntaxKind;
pub use token::*;

/// Source-level visibility on a declaration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
    Internal,
}
