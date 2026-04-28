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
    /// A floating-point literal: `3.14`, `2.5e10`.
    DoubleLit,
    /// A float literal with `f`/`F` suffix: `1.0f`, `3.14F`.
    FloatLit,
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
    ColonColon,  // :: (callable reference)
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

    /// Returns true if this token is a Kotlin keyword (reserved or soft).
    /// Used to allow keywords as member names after `.` — Kotlin permits
    /// `drawerState.open()`, `list.class`, etc.
    pub fn is_keyword(self) -> bool {
        matches!(
            self,
            TokenKind::KwFun
                | TokenKind::KwVal
                | TokenKind::KwVar
                | TokenKind::KwIf
                | TokenKind::KwElse
                | TokenKind::KwWhen
                | TokenKind::KwWhile
                | TokenKind::KwFor
                | TokenKind::KwDo
                | TokenKind::KwReturn
                | TokenKind::KwBreak
                | TokenKind::KwContinue
                | TokenKind::KwClass
                | TokenKind::KwObject
                | TokenKind::KwInterface
                | TokenKind::KwIs
                | TokenKind::KwAs
                | TokenKind::KwIn
                | TokenKind::KwNull
                | TokenKind::KwTrue
                | TokenKind::KwFalse
                | TokenKind::KwSuper
                | TokenKind::KwPackage
                | TokenKind::KwImport
                | TokenKind::KwThrow
                | TokenKind::KwTry
                | TokenKind::KwCatch
                | TokenKind::KwFinally
                | TokenKind::KwOpen
                | TokenKind::KwOverride
                | TokenKind::KwAbstract
                | TokenKind::KwPrivate
                | TokenKind::KwProtected
                | TokenKind::KwInternal
                | TokenKind::KwEnum
                | TokenKind::KwSealed
                | TokenKind::KwData
                | TokenKind::KwConst
                | TokenKind::KwLateinit
                | TokenKind::KwSuspend
                | TokenKind::KwInit
                | TokenKind::KwInfix
                | TokenKind::KwInline
                | TokenKind::KwOperator
                | TokenKind::KwVararg
                | TokenKind::KwConstructor
                | TokenKind::KwTailrec
        )
    }

    /// Return the source text for keyword tokens (e.g. `KwData` → `"data"`).
    /// Returns `None` for non-keyword tokens.
    pub fn keyword_text(&self) -> Option<&'static str> {
        match self {
            TokenKind::KwFun => Some("fun"),
            TokenKind::KwVal => Some("val"),
            TokenKind::KwVar => Some("var"),
            TokenKind::KwClass => Some("class"),
            TokenKind::KwObject => Some("object"),
            TokenKind::KwInterface => Some("interface"),
            TokenKind::KwIf => Some("if"),
            TokenKind::KwElse => Some("else"),
            TokenKind::KwWhen => Some("when"),
            TokenKind::KwWhile => Some("while"),
            TokenKind::KwFor => Some("for"),
            TokenKind::KwDo => Some("do"),
            TokenKind::KwReturn => Some("return"),
            TokenKind::KwBreak => Some("break"),
            TokenKind::KwContinue => Some("continue"),
            TokenKind::KwIn => Some("in"),
            TokenKind::KwIs => Some("is"),
            TokenKind::KwAs => Some("as"),
            TokenKind::KwTrue => Some("true"),
            TokenKind::KwFalse => Some("false"),
            TokenKind::KwNull => Some("null"),
            TokenKind::KwSuper => Some("super"),
            TokenKind::KwPackage => Some("package"),
            TokenKind::KwImport => Some("import"),
            TokenKind::KwThrow => Some("throw"),
            TokenKind::KwTry => Some("try"),
            TokenKind::KwCatch => Some("catch"),
            TokenKind::KwFinally => Some("finally"),
            TokenKind::KwOpen => Some("open"),
            TokenKind::KwOverride => Some("override"),
            TokenKind::KwAbstract => Some("abstract"),
            TokenKind::KwPrivate => Some("private"),
            TokenKind::KwProtected => Some("protected"),
            TokenKind::KwInternal => Some("internal"),
            TokenKind::KwEnum => Some("enum"),
            TokenKind::KwSealed => Some("sealed"),
            TokenKind::KwData => Some("data"),
            TokenKind::KwConst => Some("const"),
            TokenKind::KwLateinit => Some("lateinit"),
            TokenKind::KwSuspend => Some("suspend"),
            TokenKind::KwInit => Some("init"),
            TokenKind::KwInfix => Some("infix"),
            TokenKind::KwInline => Some("inline"),
            TokenKind::KwOperator => Some("operator"),
            TokenKind::KwVararg => Some("vararg"),
            TokenKind::KwConstructor => Some("constructor"),
            TokenKind::KwTailrec => Some("tailrec"),
            _ => None,
        }
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
