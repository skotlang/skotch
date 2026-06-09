//! Typed AST wrappers over [`skotch_sil::SilTree`].
//!
//! This crate provides rust-analyzer-style typed accessors over the
//! lossless SIL syntax tree. Every Kotlin construct (file, class,
//! function, expression, etc.) has a dedicated newtype that wraps a
//! `&SilNode` reference and exposes semantic accessor methods.
//!
//! ## Why a typed wrapper layer?
//!
//! Pattern-matching directly on the untyped [`SilNode`] tree is
//! noisy — every consumer has to know the exact child ordering and
//! kind expectations for every composite. The typed wrappers absorb
//! that knowledge:
//!
//! ```ignore
//! fn fun_name(f: KtFun) -> Option<&str> {
//!     f.name_token().map(|t| t.text())
//! }
//! ```
//!
//! ## Design
//!
//! - All typed nodes are zero-cost newtypes over `&SilNode`. They
//!   share the lifetime of the underlying `SilTree`, so they're
//!   `Copy` and cheap to pass around.
//! - The [`AstNode`] trait is the cast-from-[`SilNode`] surface; it
//!   matches on `kind` and (for some kinds) light validation of
//!   children.
//! - Accessor methods return `Option<…>` when a child is optional
//!   in kotlinc PSI shape, and `impl Iterator<…>` for repeated
//!   children.
//!
//! Modeled after `rowan` / rust-analyzer's `ast.rs`, but specialized
//! for our `SilNode` data structure (owned `Vec<SilNode>` children,
//! not a shared arena).
//!
//! ## End-to-end example
//!
//! ```
//! use skotch_ast::{parse, AstNode, AstToken, KtDecl, KtExpr};
//!
//! let parsed = parse("hello.kt", "fun main() { println(\"hi\") }");
//! let file = parsed.file();
//! for decl in file.decls() {
//!     if let KtDecl::Fun(f) = decl {
//!         println!("function: {:?}", f.name());
//!     }
//! }
//! ```

#![allow(clippy::needless_lifetimes)]

use skotch_sil::{SilData, SilNode};
use skotch_span::Span;
use skotch_syntax::SyntaxKind;

mod nodes;
pub use nodes::*;

/// Common interface for every typed AST node.
///
/// `cast(node)` returns `Some(typed)` only when `node.kind` matches
/// this AstNode's expected `SyntaxKind`. `syntax(self)` borrows the
/// underlying `SilNode` for callers that need to drop back to the
/// untyped layer.
pub trait AstNode<'a>: Sized + Copy {
    fn cast(node: &'a SilNode) -> Option<Self>;
    fn syntax(self) -> &'a SilNode;

    fn span(self) -> Span {
        self.syntax().span
    }

    fn kind(self) -> SyntaxKind {
        self.syntax().kind
    }
}

/// Common interface for every typed leaf (token) node.
pub trait AstToken<'a>: Sized + Copy {
    fn cast(node: &'a SilNode) -> Option<Self>;
    fn syntax(self) -> &'a SilNode;

    fn span(self) -> Span {
        self.syntax().span
    }

    fn text(self) -> &'a str {
        match &self.syntax().data {
            SilData::Token { text } => text.as_str(),
            _ => "",
        }
    }
}

// ── Tree-walking helpers ────────────────────────────────────────────

/// Iterate the children of a composite node, skipping trivia tokens
/// (`WHITE_SPACE`, comments, `NEWLINE`, KDoc). Returns the raw
/// `SilNode` slice when the caller needs to filter further.
pub fn children<'a>(node: &'a SilNode) -> &'a [SilNode] {
    match &node.data {
        SilData::Composite { children } | SilData::Error { children, .. } => children.as_slice(),
        SilData::Token { .. } => &[],
    }
}

/// Iterate the *non-trivia* children of a composite node.
pub fn non_trivia_children<'a>(node: &'a SilNode) -> impl Iterator<Item = &'a SilNode> + 'a {
    children(node).iter().filter(|c| !is_trivia(c.kind))
}

/// First non-trivia child of `kind`, or `None`.
pub fn first_child_of_kind<'a>(node: &'a SilNode, kind: SyntaxKind) -> Option<&'a SilNode> {
    children(node).iter().find(|c| c.kind == kind)
}

/// All non-trivia children of `kind`.
pub fn children_of_kind<'a>(
    node: &'a SilNode,
    kind: SyntaxKind,
) -> impl Iterator<Item = &'a SilNode> + 'a {
    children(node).iter().filter(move |c| c.kind == kind)
}

/// First typed child of type `T`, if any.
pub fn first_typed_child<'a, T: AstNode<'a>>(node: &'a SilNode) -> Option<T> {
    children(node).iter().find_map(|c| T::cast(c))
}

/// All typed children of type `T`.
pub fn typed_children<'a, T: AstNode<'a>>(node: &'a SilNode) -> impl Iterator<Item = T> + 'a {
    children(node).iter().filter_map(|c| T::cast(c))
}

/// First typed token child of type `T`, if any.
pub fn first_typed_token<'a, T: AstToken<'a>>(node: &'a SilNode) -> Option<T> {
    children(node).iter().find_map(|c| T::cast(c))
}

/// `true` for the kinds the parser emits as trivia.
pub fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITE_SPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::LINE_COMMENT
            | SyntaxKind::BLOCK_COMMENT
            | SyntaxKind::KDOC
    )
}

// ── Parse entry point ───────────────────────────────────────────────

/// Parse a Kotlin source file and return the root [`KtFile`] view.
///
/// The returned [`ParsedFile`] owns the underlying [`SilTree`]; the
/// typed wrappers borrow from it.
pub fn parse(file: impl AsRef<str>, source: &str) -> ParsedFile {
    let tree = skotch_sil::parse_sil(file.as_ref(), source);
    ParsedFile { tree }
}

/// Owner of a parsed `SilTree` with a convenient `file()` accessor.
pub struct ParsedFile {
    tree: skotch_sil::SilTree,
}

impl ParsedFile {
    pub fn root_node(&self) -> &SilNode {
        &self.tree.root
    }

    /// Typed view of the root `FILE` composite.
    pub fn file(&self) -> KtFile<'_> {
        KtFile::cast(&self.tree.root).expect("root is FILE")
    }

    pub fn tree(&self) -> &skotch_sil::SilTree {
        &self.tree
    }
}
