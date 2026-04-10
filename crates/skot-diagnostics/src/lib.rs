//! Diagnostic types and a thin wrapper over `ariadne` for source rendering.
//!
//! This crate is the only place in the workspace that imports `ariadne`.
//! Higher-layer crates push `Diagnostic`s into a `Diagnostics` sink and let
//! the CLI render them at the end of compilation.

use skot_span::{SourceMap, Span};
use std::fmt::Write as _;

/// Severity of a diagnostic. The renderer maps these to ariadne `ReportKind`s
/// and to non-zero CLI exit codes for errors.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// A primary or secondary label attached to a [`Diagnostic`].
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Label {
            span,
            message: message.into(),
        }
    }
}

/// A single compiler diagnostic. Diagnostics are immutable values; the
/// emitting code constructs them and pushes them into a [`Diagnostics`] sink.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        let message = message.into();
        Diagnostic {
            severity: Severity::Error,
            primary: Label::new(span, message.clone()),
            message,
            secondary: Vec::new(),
        }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        let message = message.into();
        Diagnostic {
            severity: Severity::Warning,
            primary: Label::new(span, message.clone()),
            message,
            secondary: Vec::new(),
        }
    }

    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }
}

/// Sink for diagnostics emitted during a compilation pass. The CLI owns
/// one of these per compilation; passes borrow it to push entries.
#[derive(Default, Debug, Clone)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.entries.push(d);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.entries.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Render the entire diagnostic sink to a `String` using a deliberately
/// plain text format. We *intentionally* don't use ariadne's source-aware
/// rendering yet — that's a follow-up once the CLI is wired up — but the
/// API surface here is shaped so the swap is local.
pub fn render(diags: &Diagnostics, sources: &SourceMap) -> String {
    let mut out = String::new();
    for d in diags.iter() {
        let kind = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        let file = sources.get(d.primary.span.file);
        let (line, col) = file.line_col(d.primary.span.start);
        let _ = writeln!(
            out,
            "{}: {} ({}:{}:{})",
            kind,
            d.message,
            file.path.display(),
            line,
            col
        );
        for s in &d.secondary {
            let f = sources.get(s.span.file);
            let (l, c) = f.line_col(s.span.start);
            let _ = writeln!(
                out,
                "  note: {} ({}:{}:{})",
                s.message,
                f.path.display(),
                l,
                c
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use skot_span::FileId;

    #[test]
    fn errors_count_toward_has_errors() {
        let mut d = Diagnostics::new();
        d.push(Diagnostic::warning(Span::empty(FileId(0)), "watch out"));
        assert!(!d.has_errors());
        d.push(Diagnostic::error(Span::empty(FileId(0)), "boom"));
        assert!(d.has_errors());
    }
}
