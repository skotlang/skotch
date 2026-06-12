//! Diagnostics sink, mirroring `android::IDiagnostics`.
//!
//! Messages are formatted like aapt2's: `path:line: error: message`.
//! The CLI prints to stderr as messages arrive; library callers can
//! collect them instead.

use crate::res::Source;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Note,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub source: Option<Source>,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(source) = &self.source {
            write!(f, "{source}: ")?;
        }
        let tag = match self.severity {
            Severity::Note => "note",
            Severity::Warn => "warn",
            Severity::Error => "error",
        };
        write!(f, "{tag}: {}", self.message)
    }
}

/// Collects (and optionally prints) diagnostics.
#[derive(Debug, Default)]
pub struct Diagnostics {
    messages: Mutex<Vec<Diagnostic>>,
    error_count: AtomicUsize,
    pub print_to_stderr: bool,
    pub verbose: bool,
}

impl Diagnostics {
    /// A sink that prints to stderr as aapt2 does.
    pub fn stderr() -> Self {
        Diagnostics { print_to_stderr: true, ..Default::default() }
    }

    /// A silent collecting sink (for library use and tests).
    pub fn collecting() -> Self {
        Diagnostics::default()
    }

    fn push(&self, diagnostic: Diagnostic) {
        if diagnostic.severity == Severity::Error {
            self.error_count.fetch_add(1, Ordering::Relaxed);
        }
        if self.print_to_stderr
            && (diagnostic.severity != Severity::Note || self.verbose)
        {
            eprintln!("{diagnostic}");
        }
        self.messages.lock().unwrap().push(diagnostic);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.push(Diagnostic { severity: Severity::Error, source: None, message: message.into() });
    }

    pub fn error_at(&self, source: Source, message: impl Into<String>) {
        self.push(Diagnostic {
            severity: Severity::Error,
            source: Some(source),
            message: message.into(),
        });
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.push(Diagnostic { severity: Severity::Warn, source: None, message: message.into() });
    }

    pub fn warn_at(&self, source: Source, message: impl Into<String>) {
        self.push(Diagnostic {
            severity: Severity::Warn,
            source: Some(source),
            message: message.into(),
        });
    }

    pub fn note(&self, message: impl Into<String>) {
        self.push(Diagnostic { severity: Severity::Note, source: None, message: message.into() });
    }

    pub fn note_at(&self, source: Source, message: impl Into<String>) {
        self.push(Diagnostic {
            severity: Severity::Note,
            source: Some(source),
            message: message.into(),
        });
    }

    pub fn has_errors(&self) -> bool {
        self.error_count.load(Ordering::Relaxed) > 0
    }

    pub fn error_count(&self) -> usize {
        self.error_count.load(Ordering::Relaxed)
    }

    pub fn take(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.messages.lock().unwrap())
    }
}
