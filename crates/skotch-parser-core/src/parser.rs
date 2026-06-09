//! Generic recursive-descent driver. Grammar lives outside this crate;
//! this holds only the plumbing every grammar needs.
//!
//! The shape is rust-analyzer's: `start()` returns a [`Marker`], which
//! you later `complete(p, kind)` to finalize as a real node, or
//! `abandon(p)` to discard. `complete` returns a [`CompletedMarker`];
//! its `precede(p)` re-opens a wrapper parent that wraps the just-
//! finished node — the precede-and-wrap trick that makes left-
//! associative parsing work without backing up.
//!
//! There is **no `Drop` panic-on-leak guard** here yet; we wire one in
//! during Phase 3 once the grammar is large enough that forgetting a
//! `complete()` becomes plausible. For now the asserts in `complete`
//! catch the easy mistakes.

use crate::event::Event;
use crate::input::Input;
use skotch_span::Span;
use skotch_syntax::{SyntaxKind, TokenKind};
use std::num::NonZeroU32;

/// Holds the running event stream while a grammar runs.
pub struct Parser<'i, 'src> {
    input: &'i Input<'src>,
    pos: usize,
    events: Vec<Event>,
    errors: Vec<String>,
}

impl<'i, 'src> Parser<'i, 'src> {
    pub fn new(input: &'i Input<'src>) -> Self {
        Self {
            input,
            pos: 0,
            events: Vec::with_capacity(input.len() * 2),
            errors: Vec::new(),
        }
    }

    pub fn input(&self) -> &Input<'src> {
        self.input
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn current(&self) -> SyntaxKind {
        self.input.kind(self.pos)
    }

    pub fn current_token_kind(&self) -> TokenKind {
        self.input.token_kind(self.pos)
    }

    pub fn current_span(&self) -> Span {
        self.input.token(self.pos).span
    }

    /// Verbatim source text of the token at the cursor. Equivalent to
    /// `self.input().text_of(self.pos())` but ergonomic enough for the
    /// many grammar predicates that examine token text — most often to
    /// detect newlines inside a `WHITE_SPACE` token (kotlinc's
    /// `WHITE_SPACE` element covers spaces *and* newlines).
    pub fn current_text(&self) -> &'src str {
        self.input.text_of(self.pos)
    }

    /// Same as [`Self::current_text`] but at an arbitrary offset.
    pub fn text_at(&self, offset: usize) -> &'src str {
        self.input.text_of(self.pos + offset)
    }

    pub fn nth(&self, n: usize) -> SyntaxKind {
        self.input.kind(self.pos + n)
    }

    pub fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub fn at_token(&self, kind: TokenKind) -> bool {
        self.current_token_kind() == kind
    }

    /// Advance past one input token and emit a [`Event::Token`].
    pub fn bump(&mut self) {
        let kind = self.current();
        if kind == SyntaxKind::EOF {
            return;
        }
        self.events.push(Event::Token {
            kind,
            n_raw_tokens: 1,
        });
        self.pos += 1;
    }

    /// Like [`Self::bump`] but emits the token with `kind` instead of
    /// the lexer's reported kind. Used when the grammar reclassifies
    /// a token in context — e.g. a `STRING_CHUNK` whose text is an
    /// escape sequence becomes an `ESCAPE_SEQUENCE` inside a
    /// `ESCAPE_STRING_TEMPLATE_ENTRY` composite.
    pub fn bump_as(&mut self, kind: SyntaxKind) {
        if self.current() == SyntaxKind::EOF {
            return;
        }
        self.events.push(Event::Token {
            kind,
            n_raw_tokens: 1,
        });
        self.pos += 1;
    }

    /// Like [`Self::bump_as`] but consumes `n` consecutive raw input
    /// tokens and merges them into a single output token with `kind`.
    /// The emitted span covers all `n` tokens; the text spans the
    /// concatenated source bytes. Used for compound operators that the
    /// lexer doesn't already fuse — `as ?` → `AS_SAFE "as?"`, for
    /// example.
    pub fn bump_n_as(&mut self, n: u8, kind: SyntaxKind) {
        if self.current() == SyntaxKind::EOF || n == 0 {
            return;
        }
        self.events.push(Event::Token {
            kind,
            n_raw_tokens: n,
        });
        self.pos += n as usize;
    }

    /// Advance only if `current() == kind`. Returns whether the bump
    /// happened.
    pub fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Open a new (kind-less) node. The returned [`Marker`] must be
    /// either `complete`d or `abandon`ed before the parser is
    /// `finish`ed.
    pub fn start(&mut self) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::tombstone());
        Marker { pos }
    }

    /// Record a parse error at the current input position.
    pub fn error(&mut self, message: impl Into<String>) {
        let idx = self.errors.len() as u32;
        self.errors.push(message.into());
        self.events.push(Event::Error { idx });
    }

    /// Finish: hand back (events, errors) for `Output::process`.
    pub fn finish(self) -> (Vec<Event>, Vec<String>) {
        (self.events, self.errors)
    }
}

/// A reserved slot in the event stream that hasn't yet decided what
/// composite node it represents.
#[must_use = "a Marker must be `complete`d or `abandon`ed"]
pub struct Marker {
    pos: u32,
}

impl Marker {
    /// Fill in the placeholder `Start` event with `kind` and append a
    /// matching `Finish`.
    pub fn complete(self, p: &mut Parser<'_, '_>, kind: SyntaxKind) -> CompletedMarker {
        let idx = self.pos as usize;
        match &mut p.events[idx] {
            Event::Start {
                kind: slot,
                forward_parent: _,
            } => {
                debug_assert_eq!(
                    *slot,
                    SyntaxKind::TOMBSTONE,
                    "Marker::complete called on a slot that was already completed"
                );
                *slot = kind;
            }
            other => unreachable!("Marker::pos points at non-Start event: {:?}", other),
        }
        p.events.push(Event::Finish);
        CompletedMarker {
            start_pos: self.pos,
            kind,
        }
    }

    /// Drop the placeholder. If it sits at the end of the stream we
    /// pop it; otherwise we leave a `TOMBSTONE` that `Output::process`
    /// skips.
    pub fn abandon(self, p: &mut Parser<'_, '_>) {
        let idx = self.pos as usize;
        if idx == p.events.len() - 1 {
            match p.events.pop() {
                Some(Event::Start {
                    kind: SyntaxKind::TOMBSTONE,
                    forward_parent: None,
                }) => {}
                other => unreachable!("abandon popped non-tombstone: {:?}", other),
            }
        }
    }
}

/// A [`Marker`] that has been `complete`d. Carries enough state to
/// retroactively wrap itself in a parent via [`Self::precede`].
pub struct CompletedMarker {
    start_pos: u32,
    kind: SyntaxKind,
}

impl CompletedMarker {
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    /// Open a new wrapper [`Marker`] *before* this completed node. When
    /// the wrapper is itself completed, the resulting tree has this
    /// node as the wrapper's child:
    ///
    /// ```text
    /// before:   [start kind=A] tok tok [finish]
    /// after:    [start kind=W ↘1] [start kind=A] tok tok [finish] [finish]
    /// ```
    ///
    /// This is how `a + b + c` becomes `((a + b) + c)` without backing
    /// up: after parsing `a + b`, we `precede` the resulting node with
    /// a new `BINARY_EXPRESSION` marker that wraps it on the way to
    /// parsing `+ c`.
    pub fn precede(self, p: &mut Parser<'_, '_>) -> Marker {
        let new_m = p.start();
        let idx = self.start_pos as usize;
        match &mut p.events[idx] {
            Event::Start { forward_parent, .. } => {
                let offset = new_m.pos - self.start_pos;
                *forward_parent = Some(
                    NonZeroU32::new(offset).expect("precede always creates a positive offset"),
                );
            }
            other => unreachable!("precede on non-Start event: {:?}", other),
        }
        new_m
    }
}
