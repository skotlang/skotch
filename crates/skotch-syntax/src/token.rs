//! Token kinds produced by [`skotch-lexer`](../../skotch-lexer).
//!
//! The lexer hands the parser a flat stream of `Token { kind, span }` plus
//! a side-table of literal payloads (interned identifiers, parsed integer
//! values, decoded string contents). Token kinds carry no payload — that
//! lives in [`Token::lexeme`] or in the payload table — so they remain
//! `Copy` and trivial to compare.

use skotch_span::Span;

/// Lexical category of a token. Carries no payload (see crate docs).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // ─── identifiers and literals ────────────────────────────────────────
    Ident,
    IntLit,
    /// A character literal: `'a'`, `'\n'`. Payload is `Int(code_point)`.
    CharLit,
    /// A `Long` literal with `L` suffix: `100L`, `0xFFL`.
    LongLit,
    /// A floating-point literal: `3.14`, `2.5e10`, `1.0f`.
    DoubleLit,
    /// A string literal *with no template interpolations* — content is
    /// already decoded and lives in the payload table. Strings that
    /// contain `$ident` or `${expr}` are emitted as a `String*` sequence
    /// (see below) which the parser stitches together.
    StringLit,

    // ─── string template tokens ──────────────────────────────────────────
    /// Opening `"` of a templated string.
    StringStart,
    /// A literal text chunk inside a templated string.
    StringChunk,
    /// `$ident` form: emitted as `StringIdentRef` carrying the identifier.
    StringIdentRef,
    /// `${` — start of an interpolated expression block. The lexer will
    /// emit normal tokens for the inner expression, terminated by
    /// `StringExprEnd`.
    StringExprStart,
    /// Matching `}` for `${ ... }`.
    StringExprEnd,
    /// Closing `"` of a templated string.
    StringEnd,

    // ─── keywords (Kotlin 2 hard keywords we currently care about) ───────
    KwFun,
    KwVal,
    KwVar,
    KwIf,
    KwElse,
    KwReturn,
    KwTrue,
    KwFalse,
    KwNull,
    KwWhile,
    KwDo,
    KwWhen,
    KwFor,
    KwIn,
    KwBreak,
    KwContinue,
    KwClass,
    KwObject,
    KwPackage,
    KwImport,
    KwConst,
    KwThrow,
    KwTry,
    KwCatch,
    KwFinally,
    KwIs,
    KwAs,
    KwSuper,
    KwInit,
    KwData,
    KwEnum,
    KwInterface,
    KwSealed,
    KwOverride,
    KwOpen,
    KwAbstract,
    KwInfix,
    KwInline,
    KwPrivate,
    KwProtected,
    KwInternal,
    KwOperator,
    KwVararg,
    KwConstructor,
    KwLateinit,
    /// `suspend` modifier on a function declaration. Recognised so the
    /// parser can accept Kotlin source that uses coroutines; the CPS
    /// transform that would make a suspend function *actually* suspend
    /// is not yet implemented (tracked in milestones.yaml v0.9.0).
    KwSuspend,
    /// `tailrec` modifier on functions. Semantically a hint that the
    /// compiler should optimize tail-recursive calls into loops.
    KwTailrec,

    // ─── single-character punctuation ────────────────────────────────────
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    Eq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    Question,
    At, // @

    // ─── multi-character punctuation ─────────────────────────────────────
    Arrow,       // ->
    EqEq,        // ==
    NotEq,       // !=
    Lt,          // <
    Gt,          // >
    LtEq,        // <=
    GtEq,        // >=
    AmpAmp,      // &&
    PipePipe,    // ||
    DotDot,      // .. (range operator)
    PlusEq,      // +=
    MinusEq,     // -=
    StarEq,      // *=
    SlashEq,     // /=
    PercentEq,   // %=
    QuestionDot, // ?.
    Elvis,       // ?:
    BangBang,    // !!
    PlusPlus,    // ++
    MinusMinus,  // --

    // ─── trivia / structural ─────────────────────────────────────────────
    /// One or more `\n`s. Kotlin treats newlines as soft statement
    /// terminators; the lexer keeps them as tokens so the parser can
    /// decide whether to consume them.
    Newline,
    /// End-of-file sentinel. Always one of these at the end of the stream.
    Eof,
    /// Lexer error: malformed input. Always carries a span pointing at
    /// the offending bytes; the parser stops on encountering one.
    Error,
}

impl TokenKind {
    /// Returns true for tokens that the parser should treat as ignorable
    /// when looking for "the next real token". Newlines are *not* trivia
    /// — Kotlin's grammar is newline-sensitive — but comments and pure
    /// whitespace would be (the lexer never produces those).
    pub fn is_trivia(self) -> bool {
        false
    }
}

/// A single token in the lexer's output stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}
