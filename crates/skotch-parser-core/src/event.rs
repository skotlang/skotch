//! Parser output events.
//!
//! The parser doesn't allocate tree nodes; it emits a flat sequence of
//! events that downstream sinks turn into whatever tree they want (the
//! SIL builder builds a lossless concrete tree, the FIR builder builds
//! a compact light-tree on the way to the typed [`skotch_syntax`] AST).
//!
//! The `Start` / `Finish` pairing mirrors rust-analyzer's parser:
//! `Start` is emitted with a `TOMBSTONE` kind that is filled in later
//! when the corresponding [`crate::Marker`] is `complete`d. This lets a
//! deeply-nested left-associative rule decide the parent kind after it
//! has finished parsing the child.
//!
//! `forward_parent` enables the precede-and-wrap trick: when parsing
//! `a + b + c`, after parsing the first `+` we want to wrap the `a + b`
//! we just built into a new `BINARY_EXPRESSION` parent so that the
//! second `+` finds `(a + b)` on its left. `forward_parent` is the
//! offset (in events) to the start event of that wrapper, and
//! [`Output::process`] follows the chain in reverse to emit the
//! appropriate nesting.

use skotch_syntax::SyntaxKind;
use std::num::NonZeroU32;

/// One step in the parser's output stream.
///
/// Errors carry their own slot rather than a `String` so the variant
/// stays small (`Event` should be a 16-byte enum on 64-bit targets,
/// fitting comfortably in a CPU cache line per dozen events).
#[derive(Debug, Clone)]
pub enum Event {
    /// A composite node opens. `kind` may start as
    /// [`SyntaxKind::TOMBSTONE`] and be patched later via
    /// [`crate::Marker::complete`].
    Start {
        kind: SyntaxKind,
        /// When `Some(n)`, the `Start` at index `self_index + n.get()`
        /// is a "wrapping parent" that should be entered *before* this
        /// node when processing events. Built by
        /// [`crate::CompletedMarker::precede`].
        forward_parent: Option<NonZeroU32>,
    },
    /// Close the most-recently-opened composite node.
    Finish,
    /// Consume one raw token from the input. `n_raw_tokens` is reserved
    /// for joined tokens (e.g. `::` lexed as one but rendered as two);
    /// Skotch currently always passes 1, but the slot exists so we can
    /// add joining without changing the event shape.
    Token { kind: SyntaxKind, n_raw_tokens: u8 },
    /// Parse error at the current input position. The actual message
    /// lives in a side table — `idx` is the index into the parser's
    /// `errors` vec.
    Error { idx: u32 },
}

impl Event {
    /// Sentinel `Start` event used when a [`crate::Marker`] is created
    /// before its kind is known.
    pub const fn tombstone() -> Self {
        Event::Start {
            kind: SyntaxKind::TOMBSTONE,
            forward_parent: None,
        }
    }
}
