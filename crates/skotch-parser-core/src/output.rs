//! Drive a parser's event stream into a [`TreeSink`].
//!
//! Resolves `forward_parent` chains and the `TOMBSTONE` placeholders
//! left behind by abandoned markers. After running, every `enter_node`
//! is balanced by a `leave_node`, and the sink has seen one `token`
//! call per non-trivia input token (or every token, when the SIL
//! pipeline is driving).
//!
//! The algorithm follows rust-analyzer's `process`:
//! - On `Start { kind, forward_parent }`, walk the `forward_parent`
//!   chain to collect the full ancestor list, then `enter_node` in
//!   reverse so wrappers open before the inner-most child.
//! - Each chain step rewrites the visited slot to `Event::tombstone()`
//!   so we don't re-enter it on the natural iteration order.
//! - `Finish` → `leave_node`.
//! - `Token` → `sink.token(kind, text, span)`. Text and span come from
//!   the [`Input`] adapter.
//! - `Error { idx }` → `sink.error(messages[idx], current span)`.
//!
//! Trivia handling: `Output::process` doesn't filter — whatever the
//! lexer emitted (and the parser bumped) reaches the sink. If you want
//! a trivia-free stream, use a FIR-path lexer (which never emits
//! whitespace/comments in the first place); if you want SIL fidelity,
//! use a `preserve_trivia: true` lexer.

use crate::event::Event;
use crate::input::Input;
use crate::sink::TreeSink;
use skotch_syntax::SyntaxKind;
use std::mem;

/// Wraps the pair `(events, errors)` returned by [`crate::Parser::finish`].
pub struct ParseOutput {
    pub events: Vec<Event>,
    pub errors: Vec<String>,
}

impl ParseOutput {
    pub fn new(events: Vec<Event>, errors: Vec<String>) -> Self {
        Self { events, errors }
    }

    /// Drive every event into `sink`. Consumes self.
    pub fn process<S: TreeSink>(self, input: &Input<'_>, sink: &mut S) {
        let ParseOutput { mut events, errors } = self;
        let mut forward_parents: Vec<SyntaxKind> = Vec::new();
        let mut input_pos: usize = 0;

        for i in 0..events.len() {
            // We may have rewritten events[i] to a tombstone while
            // following a forward-parent chain from an earlier index.
            // Skip those.
            match mem::replace(&mut events[i], Event::tombstone()) {
                Event::Start {
                    kind: SyntaxKind::TOMBSTONE,
                    forward_parent: None,
                } => {
                    // Either a real tombstone we just consumed, or a
                    // chain step already followed — either way, nothing
                    // to do.
                }
                Event::Start {
                    kind,
                    forward_parent,
                } => {
                    forward_parents.push(kind);
                    let mut fp = forward_parent;
                    let mut chain_idx = i;
                    while let Some(offset) = fp {
                        chain_idx += offset.get() as usize;
                        match mem::replace(&mut events[chain_idx], Event::tombstone()) {
                            Event::Start {
                                kind: next_kind,
                                forward_parent: next_fp,
                            } => {
                                forward_parents.push(next_kind);
                                fp = next_fp;
                            }
                            other => unreachable!(
                                "forward_parent at idx {} pointed at non-Start: {:?}",
                                chain_idx, other
                            ),
                        }
                    }
                    for kind in forward_parents.drain(..).rev() {
                        if kind != SyntaxKind::TOMBSTONE {
                            sink.enter_node(kind);
                        }
                    }
                }
                Event::Finish => sink.leave_node(),
                Event::Token { kind, n_raw_tokens } => {
                    // Emit one sink.token per *event token* but advance
                    // input by the n_raw_tokens this event covered (so
                    // joined tokens consume their constituent lexer
                    // tokens). Skotch always uses n_raw_tokens=1 today.
                    let span_start = input.token(input_pos).span;
                    let span_end = input
                        .token(input_pos + (n_raw_tokens as usize).saturating_sub(1))
                        .span;
                    let merged = skotch_span::Span {
                        file: span_start.file,
                        start: span_start.start,
                        end: span_end.end,
                    };
                    // Collect verbatim text across the n raw tokens.
                    let text = if n_raw_tokens <= 1 {
                        input.text_of(input_pos)
                    } else {
                        let src = input.source();
                        let lo = span_start.start as usize;
                        let hi = (span_end.end as usize).min(src.len());
                        &src[lo..hi]
                    };
                    sink.token(kind, text, merged);
                    input_pos += n_raw_tokens as usize;
                }
                Event::Error { idx } => {
                    let msg = errors.get(idx as usize).cloned().unwrap_or_default();
                    let span = input.token(input_pos).span;
                    sink.error(&msg, span);
                }
            }
        }
    }
}
