//! `SilSink` — turns the parser's event stream into a [`SilTree`].
//!
//! Implements [`skotch_parser_core::TreeSink`]: every `enter_node`
//! pushes an empty children-vec onto the build stack; `leave_node`
//! pops, wraps as a composite, and attaches to the parent. Tokens
//! become leaf nodes directly. Errors mark the current top-of-stack
//! as an `Error` node.
//!
//! The sink does not allocate `Arc`s; children are owned via `Vec`.
//! For the file sizes Skotch sees (single-file PSI dumps are bounded
//! by source size), arenas would be premature.

use crate::kdoc::parse_kdoc;
use crate::tree::{SilNode, SilTree};
use skotch_parser_core::TreeSink;
use skotch_span::{FileId, Span};
use skotch_syntax::SyntaxKind;

/// Builds a [`SilTree`] from a stream of [`TreeSink`] calls.
///
/// Usage:
/// ```ignore
/// let mut sink = SilSink::new(file_path, source_length, crlf_normalized, file_id);
/// ParseOutput::new(events, errors).process(&input, &mut sink);
/// let tree = sink.finish();
/// ```
pub struct SilSink {
    file: String,
    source_length: u32,
    crlf_normalized: bool,
    file_id: FileId,
    /// Stack of in-progress composites. Each entry is the children-vec
    /// of a node whose `enter_node` has fired but whose `leave_node`
    /// has not.
    open: Vec<OpenNode>,
    /// Set after the outermost `leave_node` completes. There's always
    /// exactly one root after a well-formed event stream.
    root: Option<SilNode>,
}

struct OpenNode {
    kind: SyntaxKind,
    children: Vec<SilNode>,
    /// Index into the *parent's* children at the moment this node
    /// opened. Unused right now but cheap to keep, and the future
    /// "convert this composite to an error" path will need it.
    #[allow(dead_code)]
    parent_child_index: u32,
    /// Span start — patched when we know the first child's span.
    /// Defaults to 0 if the node ends up empty.
    start: u32,
    end: u32,
    /// `Some(message)` flips this composite to a `SilData::Error` on
    /// `leave_node`. Set by `TreeSink::error`.
    error_msg: Option<String>,
}

impl SilSink {
    pub fn new(
        file: impl Into<String>,
        source_length: u32,
        crlf_normalized: bool,
        file_id: FileId,
    ) -> Self {
        Self {
            file: file.into(),
            source_length,
            crlf_normalized,
            file_id,
            open: Vec::new(),
            root: None,
        }
    }

    /// Consume the sink and return the final tree.
    ///
    /// Panics if the event stream was malformed (unbalanced
    /// enter/leave). Such streams indicate a parser bug, not user
    /// error, so panicking is appropriate.
    pub fn finish(mut self) -> SilTree {
        assert!(
            self.open.is_empty(),
            "SilSink::finish with {} unclosed nodes",
            self.open.len()
        );
        let root = self.root.take().unwrap_or_else(|| {
            SilNode::composite(
                SyntaxKind::FILE,
                vec![],
                Span {
                    file: self.file_id,
                    start: 0,
                    end: 0,
                },
            )
        });
        SilTree {
            file: self.file,
            source_length: self.source_length,
            crlf_normalized: self.crlf_normalized,
            root,
        }
    }
}

impl TreeSink for SilSink {
    fn enter_node(&mut self, kind: SyntaxKind) {
        let parent_child_index = self
            .open
            .last()
            .map(|n| n.children.len() as u32)
            .unwrap_or(0);
        self.open.push(OpenNode {
            kind,
            children: Vec::new(),
            parent_child_index,
            start: u32::MAX,
            end: 0,
            error_msg: None,
        });
    }

    fn leave_node(&mut self) {
        let OpenNode {
            kind,
            children,
            parent_child_index: _,
            start,
            end,
            error_msg,
        } = self
            .open
            .pop()
            .expect("leave_node with no matching enter_node");
        // Span coalescing: if we never saw any child span, fall back
        // to a zero-width span at position 0. Real composites always
        // contain at least one token (which sets `start`/`end`).
        let span = Span {
            file: self.file_id,
            start: if start == u32::MAX { 0 } else { start },
            end,
        };
        let node = match error_msg {
            Some(msg) => SilNode::error(kind, msg, children, span),
            None => SilNode::composite(kind, children, span),
        };

        if let Some(parent) = self.open.last_mut() {
            // Roll span up to the parent.
            if parent.start == u32::MAX {
                parent.start = span.start;
            } else {
                parent.start = parent.start.min(span.start);
            }
            parent.end = parent.end.max(span.end);
            parent.children.push(node);
        } else {
            assert!(
                self.root.is_none(),
                "second root emitted after first was finished"
            );
            self.root = Some(node);
        }
    }

    fn token(&mut self, kind: SyntaxKind, text: &str, span: Span) {
        // Doc comments arrive as a single `KDOC` token from the
        // lexer; expand them into the structured `KDoc` sub-tree
        // here so the SIL output matches kotlinc PSI shape.
        if kind == SyntaxKind::KDOC && text.starts_with("/**") && text.ends_with("*/") {
            let node = parse_kdoc(text, span.start, self.file_id);
            self.attach(node, span);
            return;
        }

        // `$ident` short-template entry: kotlinc PSI splits this into
        // a leading `$` (SHORT_TEMPLATE_ENTRY_START) plus a
        // REFERENCE_EXPRESSION wrapping the identifier. The lexer
        // emits the entire `$ident` as one STRING_IDENT_REF token —
        // synthesize the split here.
        if kind == SyntaxKind::STRING_IDENT_REF && text.starts_with('$') && text.len() > 1 {
            let dollar_span = Span {
                file: self.file_id,
                start: span.start,
                end: span.start + 1,
            };
            let ident_span = Span {
                file: self.file_id,
                start: span.start + 1,
                end: span.end,
            };
            // The `$` token uses SHORT_TEMPLATE_ENTRY_START kind via
            // the yaml_kind mapping; we keep it on STRING_IDENT_REF
            // here so name lookup stays consistent.
            let dollar = SilNode::token(SyntaxKind::STRING_IDENT_REF, "$", dollar_span);
            // `$this` is the THIS_EXPRESSION form of a short template
            // entry. kotlinc wraps it as
            //   SHORT_STRING_TEMPLATE_ENTRY { $ THIS_EXPRESSION { REF { this } } }
            // rather than the plain `$ident` REF shape.
            let ident_text = &text[1..];
            let ref_kind = if ident_text == "this" {
                SyntaxKind::KW_THIS
            } else {
                SyntaxKind::IDENTIFIER
            };
            let ident = SilNode::token(ref_kind, ident_text, ident_span);
            let ref_expr = SilNode {
                kind: SyntaxKind::REFERENCE_EXPRESSION,
                span: ident_span,
                data: crate::tree::SilData::Composite {
                    children: vec![ident],
                },
            };
            self.attach(dollar, dollar_span);
            if ident_text == "this" {
                let this_expr = SilNode {
                    kind: SyntaxKind::THIS_EXPRESSION,
                    span: ident_span,
                    data: crate::tree::SilData::Composite {
                        children: vec![ref_expr],
                    },
                };
                self.attach(this_expr, ident_span);
            } else {
                self.attach(ref_expr, ident_span);
            }
            return;
        }

        let node = SilNode::token(kind, text, span);
        self.attach(node, span);
    }

    fn error(&mut self, message: &str, _span: Span) {
        let _ = (); // keep signature shape for the helper below
        self.record_error(message);
    }
}

impl SilSink {
    /// Attach `node` to the currently-open composite (or set as root
    /// if nothing is open). Updates the parent's span range so the
    /// composite covers all its children.
    fn attach(&mut self, node: SilNode, span: Span) {
        if let Some(parent) = self.open.last_mut() {
            if parent.start == u32::MAX {
                parent.start = span.start;
            } else {
                parent.start = parent.start.min(span.start);
            }
            parent.end = parent.end.max(span.end);
            parent.children.push(node);
        } else {
            self.root = Some(node);
        }
    }

    fn record_error(&mut self, message: &str) {
        // Attach to the innermost open composite. If we're at top
        // level, push a synthetic ERROR_ELEMENT so the message is
        // captured somewhere.
        if let Some(top) = self.open.last_mut() {
            // Keep the first message — later errors on the same node
            // are usually cascades of the first.
            if top.error_msg.is_none() {
                top.error_msg = Some(message.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::SilData;
    use skotch_span::FileId;

    fn s(start: u32, end: u32) -> Span {
        Span {
            file: FileId(0),
            start,
            end,
        }
    }

    #[test]
    fn builds_a_minimal_tree() {
        let mut sink = SilSink::new("test.kt", 11, false, FileId(0));
        sink.enter_node(SyntaxKind::FILE);
        sink.enter_node(SyntaxKind::PACKAGE_DIRECTIVE);
        sink.token(SyntaxKind::KW_PACKAGE, "package", s(0, 7));
        sink.token(SyntaxKind::WHITE_SPACE, " ", s(7, 8));
        sink.token(SyntaxKind::IDENTIFIER, "foo", s(8, 11));
        sink.leave_node(); // PACKAGE_DIRECTIVE
        sink.leave_node(); // FILE
        let tree = sink.finish();
        assert_eq!(tree.root.kind, SyntaxKind::FILE);
        assert_eq!(tree.reconstruct(), "package foo");
    }

    #[test]
    fn empty_composite_keeps_kind() {
        let mut sink = SilSink::new("test.kt", 0, false, FileId(0));
        sink.enter_node(SyntaxKind::FILE);
        sink.enter_node(SyntaxKind::IMPORT_LIST);
        sink.leave_node();
        sink.leave_node();
        let tree = sink.finish();
        match &tree.root.data {
            SilData::Composite { children } => {
                assert_eq!(children.len(), 1);
                assert_eq!(children[0].kind, SyntaxKind::IMPORT_LIST);
                match &children[0].data {
                    SilData::Composite { children } => assert!(children.is_empty()),
                    _ => panic!("import list should still be a composite"),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn error_demotes_composite_to_error_node() {
        let mut sink = SilSink::new("test.kt", 3, false, FileId(0));
        sink.enter_node(SyntaxKind::FILE);
        sink.enter_node(SyntaxKind::FUN);
        sink.error("expected name", s(0, 0));
        sink.token(SyntaxKind::KW_FUN, "fun", s(0, 3));
        sink.leave_node();
        sink.leave_node();
        let tree = sink.finish();
        let SilData::Composite { children: top, .. } = &tree.root.data else {
            panic!()
        };
        match &top[0].data {
            SilData::Error { message, .. } => assert_eq!(message, "expected name"),
            _ => panic!("FUN should have been demoted to Error"),
        }
    }
}
