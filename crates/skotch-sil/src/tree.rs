//! Lossless concrete syntax tree for Kotlin.
//!
//! Every leaf carries its verbatim source text. Concatenating all leaf
//! `text` fields in pre-order reproduces the original source byte-for-
//! byte (after the CRLF→LF normalization the parser applies). This is
//! the load-bearing invariant the YAML roundtrip relies on.
//!
//! Composites are `(kind, children: Vec<SilNode>)`; tokens are
//! `(kind, text)`; error markers carry both children and a message.
//! The whole tree is owned (no `Arc` sharing) — file sizes that matter
//! to Skotch are below the threshold where shared subtrees pay off.

use skotch_span::Span;
use skotch_syntax::SyntaxKind;

/// A single node in the concrete syntax tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SilNode {
    pub kind: SyntaxKind,
    pub span: Span,
    pub data: SilData,
}

/// Per-node payload: token text, child list, or error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SilData {
    /// A leaf token — the YAML emitter writes this as `text: "..."`.
    Token { text: String },
    /// A composite node — `children: [...]` in YAML. May be empty
    /// (e.g. `IMPORT_LIST` with no imports).
    Composite { children: Vec<SilNode> },
    /// A `PsiErrorElement` equivalent: a region the parser couldn't
    /// fit into the grammar. Still carries children so the source
    /// roundtrip succeeds.
    Error {
        message: String,
        children: Vec<SilNode>,
    },
}

impl SilNode {
    pub fn token(kind: SyntaxKind, text: impl Into<String>, span: Span) -> Self {
        SilNode {
            kind,
            span,
            data: SilData::Token { text: text.into() },
        }
    }

    pub fn composite(kind: SyntaxKind, children: Vec<SilNode>, span: Span) -> Self {
        SilNode {
            kind,
            span,
            data: SilData::Composite { children },
        }
    }

    pub fn error(
        kind: SyntaxKind,
        message: impl Into<String>,
        children: Vec<SilNode>,
        span: Span,
    ) -> Self {
        SilNode {
            kind,
            span,
            data: SilData::Error {
                message: message.into(),
                children,
            },
        }
    }

    /// `true` if this node has no children — either a token leaf or
    /// an empty composite.
    pub fn is_leaf(&self) -> bool {
        match &self.data {
            SilData::Token { .. } => true,
            SilData::Composite { children } => children.is_empty(),
            SilData::Error { children, .. } => children.is_empty(),
        }
    }

    /// Append every leaf's verbatim `text` to `out` in pre-order. The
    /// load-bearing invariant: `collect_text(root)` returns the
    /// original source.
    pub fn collect_text(&self, out: &mut String) {
        match &self.data {
            SilData::Token { text } => out.push_str(text),
            SilData::Composite { children } | SilData::Error { children, .. } => {
                for c in children {
                    c.collect_text(out);
                }
            }
        }
    }

    /// Convenience: the full reconstructed source from this node down.
    pub fn reconstruct(&self) -> String {
        let mut s = String::new();
        self.collect_text(&mut s);
        s
    }
}

/// A complete parsed source file: the root [`SilNode`] (always a
/// `FILE` composite) plus the metadata the YAML serializer needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SilTree {
    /// Display path written into the `file:` YAML field. The parser
    /// puts the absolute or workspace-relative path here.
    pub file: String,
    /// Length of the normalized source (after CRLF→LF). The YAML
    /// `source_length` field. Used by the validator to assert the
    /// reconstructed source has the same byte count.
    pub source_length: u32,
    /// `true` if the on-disk source had any CRLF or CR sequences that
    /// were normalized to LF before parsing. Captured so the
    /// `reconstruct → write to disk` flow knows whether to write LF
    /// or restore CRLF.
    pub crlf_normalized: bool,
    /// The root of the tree. Always a `FILE`-kind composite.
    pub root: SilNode,
}

impl SilTree {
    /// Concatenate every leaf's text in pre-order.
    pub fn reconstruct(&self) -> String {
        self.root.reconstruct()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_span::{FileId, Span};

    fn span(s: u32, e: u32) -> Span {
        Span {
            file: FileId(0),
            start: s,
            end: e,
        }
    }

    #[test]
    fn collect_text_concatenates_leaves() {
        // FILE: [ PACKAGE_DIRECTIVE: [ KW_PACKAGE "package", WHITE_SPACE " ", REF "foo" ] ]
        let pkg = SilNode::composite(
            SyntaxKind::PACKAGE_DIRECTIVE,
            vec![
                SilNode::token(SyntaxKind::KW_PACKAGE, "package", span(0, 7)),
                SilNode::token(SyntaxKind::WHITE_SPACE, " ", span(7, 8)),
                SilNode::token(SyntaxKind::IDENTIFIER, "foo", span(8, 11)),
            ],
            span(0, 11),
        );
        let file = SilNode::composite(SyntaxKind::FILE, vec![pkg], span(0, 11));
        assert_eq!(file.reconstruct(), "package foo");
    }

    #[test]
    fn empty_composite_contributes_no_text() {
        let imports = SilNode::composite(SyntaxKind::IMPORT_LIST, vec![], span(0, 0));
        assert_eq!(imports.reconstruct(), "");
        assert!(imports.is_leaf());
    }

    #[test]
    fn error_node_still_contributes_children_text() {
        let err = SilNode::error(
            SyntaxKind::ERROR_ELEMENT,
            "boom",
            vec![SilNode::token(SyntaxKind::IDENTIFIER, "x", span(0, 1))],
            span(0, 1),
        );
        assert_eq!(err.reconstruct(), "x");
    }
}
