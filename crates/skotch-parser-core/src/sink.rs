//! `TreeSink`: the trait every backend tree builder implements.
//!
//! `Output::process` walks the parser's [`Event`] stream and turns each
//! event into a [`TreeSink`] call. Implementors decide what shape of
//! tree (if any) to build — `SilSink` builds a lossless concrete
//! syntax tree, `LightTreeSink` builds the flat arena the FIR builder
//! walks, and `DebugSink` (in tests) just prints what it gets.

use skotch_span::Span;
use skotch_syntax::SyntaxKind;

/// Receives a normalized stream of tree-building calls.
///
/// All methods are `&mut self` and called in strict pre-order. A
/// well-formed driver guarantees every `enter_node` is balanced by
/// exactly one `leave_node`, and that `token` is never called without
/// a currently-open node (the implicit root is the `FILE` opened at
/// stream start).
///
/// The driver is responsible for resolving `forward_parent` chains
/// before reaching the sink — implementors never see `TOMBSTONE`.
pub trait TreeSink {
    /// Open a composite node of the given kind.
    fn enter_node(&mut self, kind: SyntaxKind);
    /// Close the most-recently-opened composite node.
    fn leave_node(&mut self);
    /// Emit a leaf token with the given verbatim source text and span.
    fn token(&mut self, kind: SyntaxKind, text: &str, span: Span);
    /// Record a parse error attached to the current input position.
    /// `span` covers the offending token(s).
    fn error(&mut self, message: &str, span: Span);
}

/// Minimal sink used by tests — counts how many of each call landed.
/// Lives in the core crate so other crates' integration tests can also
/// use it without pulling in a sink that has tree-building dependencies.
#[derive(Default, Debug)]
pub struct CountingSink {
    pub enter_count: usize,
    pub leave_count: usize,
    pub token_count: usize,
    pub error_count: usize,
    pub kinds_entered: Vec<SyntaxKind>,
    pub tokens_emitted: Vec<(SyntaxKind, String)>,
}

impl TreeSink for CountingSink {
    fn enter_node(&mut self, kind: SyntaxKind) {
        self.enter_count += 1;
        self.kinds_entered.push(kind);
    }
    fn leave_node(&mut self) {
        self.leave_count += 1;
    }
    fn token(&mut self, kind: SyntaxKind, text: &str, _span: Span) {
        self.token_count += 1;
        self.tokens_emitted.push((kind, text.to_string()));
    }
    fn error(&mut self, _message: &str, _span: Span) {
        self.error_count += 1;
    }
}
