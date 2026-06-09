//! Event-emitting parser plumbing shared by Skotch's FIR and SIL
//! Kotlin grammars.
//!
//! See the type-level docs on each module for details; in short:
//!
//! 1. A grammar takes a `&mut [Parser]` and emits [`Event`]s by calling
//!    [`Parser::start`], [`Parser::bump`], [`Parser::error`], etc.
//! 2. The grammar finishes by calling [`Parser::finish`] which yields
//!    `(Vec<Event>, Vec<String>)`.
//! 3. A driver wraps those in [`ParseOutput`] and calls
//!    [`ParseOutput::process`] with a chosen [`TreeSink`] implementation.
//!    The FIR build path's sink produces a compact light-tree; the SIL
//!    path's sink produces a lossless concrete tree.
//!
//! This crate intentionally knows nothing about Kotlin grammar. That
//! lives in `skotch-parser-grammar` (created during Phase 3).

mod event;
mod input;
mod output;
mod parser;
mod sink;

pub use event::Event;
pub use input::Input;
pub use output::ParseOutput;
pub use parser::{CompletedMarker, Marker, Parser};
pub use sink::{CountingSink, TreeSink};

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_diagnostics::Diagnostics;
    use skotch_lexer::lex;
    use skotch_span::FileId;
    use skotch_syntax::SyntaxKind;

    fn drive<F: FnOnce(&mut Parser<'_, '_>)>(src: &str, grammar: F) -> CountingSink {
        let mut diags = Diagnostics::new();
        let lexed = lex(FileId(0), src, &mut diags);
        let input = Input::new(&lexed, src);
        let mut parser = Parser::new(&input);
        grammar(&mut parser);
        let (events, errors) = parser.finish();
        let mut sink = CountingSink::default();
        ParseOutput::new(events, errors).process(&input, &mut sink);
        sink
    }

    #[test]
    fn empty_grammar_emits_no_events() {
        let sink = drive("", |_p| {});
        assert_eq!(sink.enter_count, 0);
        assert_eq!(sink.leave_count, 0);
        assert_eq!(sink.token_count, 0);
    }

    #[test]
    fn start_complete_emits_balanced_enter_leave() {
        let sink = drive("fun foo()", |p| {
            let m = p.start();
            p.bump(); // KW_FUN
            p.bump(); // IDENTIFIER
            p.bump(); // LPAR
            p.bump(); // RPAR
            m.complete(p, SyntaxKind::FUN);
        });
        assert_eq!(sink.enter_count, 1);
        assert_eq!(sink.leave_count, 1);
        assert_eq!(sink.token_count, 4);
        assert_eq!(sink.kinds_entered, vec![SyntaxKind::FUN]);
        assert_eq!(
            sink.tokens_emitted,
            vec![
                (SyntaxKind::KW_FUN, "fun".to_string()),
                (SyntaxKind::IDENTIFIER, "foo".to_string()),
                (SyntaxKind::LPAR, "(".to_string()),
                (SyntaxKind::RPAR, ")".to_string()),
            ]
        );
    }

    #[test]
    fn abandon_does_not_emit_enter_leave() {
        let sink = drive("foo", |p| {
            let m = p.start();
            p.bump();
            m.abandon(p);
        });
        // The marker was abandoned without a kind, so no enter/leave
        // should reach the sink, but the bumped token still does.
        assert_eq!(sink.enter_count, 0);
        assert_eq!(sink.leave_count, 0);
        assert_eq!(sink.token_count, 1);
    }

    #[test]
    fn precede_wraps_completed_node() {
        // Simulate `a + b`: parse `a` as a REFERENCE_EXPRESSION, then
        // precede with a BINARY_EXPRESSION wrapper, bump the `+`, then
        // parse `b` and complete the wrapper.
        let sink = drive("a+b", |p| {
            // a
            let m = p.start();
            p.bump(); // IDENTIFIER "a"
            let a_cm = m.complete(p, SyntaxKind::REFERENCE_EXPRESSION);

            // precede with BINARY_EXPRESSION wrapping `a`
            let bin = a_cm.precede(p);
            p.bump(); // PLUS

            // b
            let m_b = p.start();
            p.bump(); // IDENTIFIER "b"
            m_b.complete(p, SyntaxKind::REFERENCE_EXPRESSION);

            bin.complete(p, SyntaxKind::BINARY_EXPRESSION);
        });

        // Expected event ordering after forward-parent resolution:
        // enter BINARY_EXPRESSION
        //   enter REFERENCE_EXPRESSION (a)
        //     token IDENTIFIER "a"
        //   leave
        //   token PLUS "+"
        //   enter REFERENCE_EXPRESSION (b)
        //     token IDENTIFIER "b"
        //   leave
        // leave
        assert_eq!(
            sink.kinds_entered,
            vec![
                SyntaxKind::BINARY_EXPRESSION,
                SyntaxKind::REFERENCE_EXPRESSION,
                SyntaxKind::REFERENCE_EXPRESSION,
            ]
        );
        assert_eq!(sink.enter_count, 3);
        assert_eq!(sink.leave_count, 3);
        assert_eq!(sink.token_count, 3); // a, +, b
    }

    #[test]
    fn error_reaches_sink() {
        let sink = drive("foo", |p| {
            p.error("expected fun");
            p.bump();
        });
        assert_eq!(sink.error_count, 1);
        assert_eq!(sink.token_count, 1);
    }
}
