//! Token kinds and AST node types for skot.
//!
//! This crate is pure data: no parsing logic, no resolution, no traversal
//! helpers beyond what's necessary to construct nodes. The lexer and
//! parser depend on this crate; the IR / type-check / backend layers do
//! not — they consume the AST through the parser's output type.

pub mod ast;
pub mod token;

pub use ast::*;
pub use token::*;
