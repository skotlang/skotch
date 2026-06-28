//! Concrete syntax kinds shared between FIR and SIL parse paths.
//!
//! Inspired by rust-analyzer's `parser::SyntaxKind` and kotlinc's
//! `KtNodeTypes` + `KtTokens`. One enum covers *both* composite nodes
//! (the things that have children) and leaf tokens (the things that
//! carry text). Disambiguate at runtime with [`SyntaxKind::is_token`] /
//! [`SyntaxKind::is_composite`].
//!
//! ## Naming
//!
//! Composite kinds match kotlinc's IElementType `toString()` output
//! verbatim (`FUN`, `CLASS`, `BINARY_EXPRESSION`, `DOT_QUALIFIED_EXPRESSION`,
//! …). Leaf-token kinds re-use the existing [`crate::TokenKind`] names
//! and are produced by [`SyntaxKind::from_token_kind`] when the parser
//! emits a token event.
//!
//! ## Scope
//!
//! The composite list covers what the SIL-emitting pipeline currently
//! needs to round-trip the `~/Desktop/psi.yaml` reference plus every
//! construct the FIR parser produces today (see
//! `skotch-parser/src/lib.rs`). Additional kinds — `IS_EXPRESSION`,
//! `ANONYMOUS_INITIALIZER`, etc. — are added as Phase 3 progresses and
//! we discover more grammar forms.

use crate::TokenKind;

/// Every kind a [`crate::Token`] or composite syntax node can have.
///
/// Variants are grouped into four blocks: special, composite, token,
/// and KDoc. Helpers below project a kind to "is this a leaf?" or "is
/// this trivia?" without enumerating the variants by hand.
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[allow(non_camel_case_types)]
pub enum SyntaxKind {
    // ─── special ────────────────────────────────────────────────────────
    /// Placeholder reserved for in-flight parser markers; never appears
    /// in a finished tree. Matches rust-analyzer's `TOMBSTONE`.
    TOMBSTONE,
    /// End-of-file marker. Always the last token in a stream.
    EOF,
    /// Synthetic node wrapping a region the parser could not understand.
    /// Children are still recorded so the SIL roundtrip succeeds; the
    /// FIR builder treats them as malformed.
    ERROR_ELEMENT,

    // ─── composite — file / packages / imports ──────────────────────────
    /// Root of every parsed file (matches kotlinc's `kotlin.FILE`).
    FILE,
    PACKAGE_DIRECTIVE,
    IMPORT_LIST,
    IMPORT_DIRECTIVE,
    IMPORT_ALIAS,

    // ─── composite — declarations ───────────────────────────────────────
    CLASS,
    OBJECT_DECLARATION,
    COMPANION_OBJECT,
    INTERFACE,
    ENUM_CLASS,
    ENUM_ENTRY,
    TYPEALIAS,
    FUN,
    PROPERTY,
    PROPERTY_ACCESSOR,
    PRIMARY_CONSTRUCTOR,
    SECONDARY_CONSTRUCTOR,
    CONSTRUCTOR_DELEGATION_CALL,
    CONSTRUCTOR_DELEGATION_REFERENCE,
    CLASS_BODY,
    ANONYMOUS_INITIALIZER,

    // ─── composite — modifiers / annotations ────────────────────────────
    MODIFIER_LIST,
    ANNOTATION,
    ANNOTATION_ENTRY,
    ANNOTATION_USE_SITE_TARGET,
    /// File-level annotation list, e.g. `@file:Suppress(...)`. Sits at
    /// FILE level before the PACKAGE_DIRECTIVE.
    FILE_ANNOTATION_LIST,

    // ─── composite — parameters / arguments ─────────────────────────────
    VALUE_PARAMETER_LIST,
    VALUE_PARAMETER,
    VALUE_ARGUMENT_LIST,
    VALUE_ARGUMENT,
    VALUE_ARGUMENT_NAME,
    LAMBDA_ARGUMENT,

    // ─── composite — types ──────────────────────────────────────────────
    TYPE_PARAMETER_LIST,
    TYPE_PARAMETER,
    TYPE_ARGUMENT_LIST,
    TYPE_PROJECTION,
    TYPE_REFERENCE,
    USER_TYPE,
    NULLABLE_TYPE,
    FUNCTION_TYPE,
    FUNCTION_TYPE_RECEIVER,
    DYNAMIC_TYPE,
    TYPE_CONSTRAINT_LIST,
    TYPE_CONSTRAINT,

    // ─── composite — supertypes ─────────────────────────────────────────
    SUPER_TYPE_LIST,
    SUPER_TYPE_ENTRY,
    DELEGATED_SUPER_TYPE_ENTRY,
    SUPER_TYPE_CALL_ENTRY,
    CONSTRUCTOR_CALLEE,

    // ─── composite — statements / control flow ──────────────────────────
    BLOCK,
    IF,
    THEN,
    ELSE,
    /// Wraps the body of a `for`/`while`/`do` loop — kotlinc PSI
    /// places the BLOCK/statement under a `BODY` composite.
    BODY,
    WHEN,
    WHEN_ENTRY,
    WHEN_CONDITION_IN_RANGE,
    WHEN_CONDITION_IS_PATTERN,
    WHEN_CONDITION_WITH_EXPRESSION,
    CONDITION,
    FOR,
    WHILE,
    DO_WHILE,
    TRY,
    CATCH,
    FINALLY,
    RETURN,
    THROW,
    BREAK,
    CONTINUE,
    DESTRUCTURING_DECLARATION,
    DESTRUCTURING_DECLARATION_ENTRY,
    LABELED_STATEMENT,
    /// `@label` qualifier on `this`, `super`, `return`, `break`, or
    /// `continue` — a LABEL composite wrapped in LABEL_QUALIFIER.
    LABEL_QUALIFIER,
    LABEL,
    /// `for (x in <expr>)` — the iterable expression inside the
    /// for-loop header is wrapped in LOOP_RANGE by kotlinc PSI.
    LOOP_RANGE,

    // ─── composite — expressions ────────────────────────────────────────
    BINARY_EXPRESSION,
    BINARY_WITH_TYPE_RHS_EXPRESSION,
    PREFIX_EXPRESSION,
    POSTFIX_EXPRESSION,
    UNARY_EXPRESSION,
    OPERATION_REFERENCE,
    DOT_QUALIFIED_EXPRESSION,
    SAFE_ACCESS_EXPRESSION,
    REFERENCE_EXPRESSION,
    THIS_EXPRESSION,
    SUPER_EXPRESSION,
    CALL_EXPRESSION,
    LAMBDA_EXPRESSION,
    FUNCTION_LITERAL,
    ARRAY_ACCESS_EXPRESSION,
    INDICES,
    CALLABLE_REFERENCE_EXPRESSION,
    CLASS_LITERAL_EXPRESSION,
    PARENTHESIZED,
    COLLECTION_LITERAL_EXPRESSION,
    ANNOTATED_EXPRESSION,
    LABELED_EXPRESSION,
    IS_EXPRESSION,
    OBJECT_LITERAL,

    // ─── composite — string templates ───────────────────────────────────
    STRING_TEMPLATE,
    LITERAL_STRING_TEMPLATE_ENTRY,
    ESCAPE_STRING_TEMPLATE_ENTRY,
    SHORT_STRING_TEMPLATE_ENTRY,
    LONG_STRING_TEMPLATE_ENTRY,
    BLOCK_STRING_TEMPLATE_ENTRY,

    // ─── composite — constants ──────────────────────────────────────────
    INTEGER_CONSTANT,
    FLOAT_CONSTANT,
    BOOLEAN_CONSTANT,
    CHARACTER_CONSTANT,
    NULL_CONSTANT,

    // ─── leaf-token kinds (1:1 with [`TokenKind`]) ──────────────────────
    //
    // The parser's event stream emits these via [`SyntaxKind::from_token_kind`].
    // Mirrors TokenKind so that downstream code working with a SyntaxKind
    // can match without crossing the SyntaxKind / TokenKind boundary.
    IDENTIFIER,
    INTEGER_LITERAL,
    CHARACTER_LITERAL,
    LONG_LITERAL,
    DOUBLE_LITERAL,
    FLOAT_LITERAL,
    STRING_LITERAL,
    STRING_START,
    STRING_CHUNK,
    STRING_IDENT_REF,
    STRING_EXPR_START,
    STRING_EXPR_END,
    STRING_END,
    REGULAR_STRING_PART,
    OPEN_QUOTE,
    CLOSING_QUOTE,
    /// A leaf token representing a backslash-escape inside a string
    /// (`\n`, `\t`, `\\`, `\xHH`, …). Distinguishes escape content
    /// from plain `REGULAR_STRING_PART` so the SIL output matches
    /// kotlinc PSI's `ESCAPE_STRING_TEMPLATE_ENTRY` shape.
    ESCAPE_SEQUENCE,

    // keywords (lowercase IElementType `toString()` to match psi.yaml)
    KW_FUN,
    KW_VAL,
    KW_VAR,
    KW_IF,
    KW_ELSE,
    KW_RETURN,
    KW_TRUE,
    KW_FALSE,
    KW_NULL,
    KW_WHILE,
    KW_DO,
    KW_WHEN,
    KW_FOR,
    KW_IN,
    KW_BREAK,
    KW_CONTINUE,
    KW_CLASS,
    KW_OBJECT,
    KW_PACKAGE,
    KW_IMPORT,
    KW_CONST,
    KW_THROW,
    KW_TRY,
    KW_CATCH,
    KW_FINALLY,
    KW_IS,
    KW_AS,
    KW_SUPER,
    KW_INIT,
    KW_DATA,
    KW_ENUM,
    KW_INTERFACE,
    KW_SEALED,
    KW_OVERRIDE,
    KW_OPEN,
    KW_ABSTRACT,
    KW_INFIX,
    KW_INLINE,
    KW_PRIVATE,
    KW_PROTECTED,
    KW_INTERNAL,
    KW_OPERATOR,
    KW_VARARG,
    KW_CONSTRUCTOR,
    KW_LATEINIT,
    KW_SUSPEND,
    KW_TAILREC,
    KW_THIS,
    KW_TYPEALIAS,
    KW_BY,
    KW_OUT,
    KW_WHERE,
    KW_ANNOTATION,
    KW_COMPANION,
    KW_REIFIED,
    KW_VALUE,
    KW_CROSSINLINE,
    KW_NOINLINE,
    KW_ACTUAL,
    KW_EXPECT,
    KW_FIELD,
    KW_GET,
    KW_SET,
    KW_PARAM,
    KW_PROPERTY,
    KW_RECEIVER,
    KW_FILE,
    KW_FUN_INTERFACE, // not a real token; placeholder for `fun interface`
    /// `public` visibility modifier. The lexer surfaces this as
    /// IDENTIFIER (it isn't a hard keyword); the SIL grammar uses
    /// `bump_as` to reclassify it inside MODIFIER_LIST.
    KW_PUBLIC,
    /// `inner class …` modifier. Same soft-keyword story as `public`.
    KW_INNER,

    // punctuation
    LPAR,
    RPAR,
    LBRACE,
    RBRACE,
    LBRACKET,
    RBRACKET,
    COMMA,
    SEMICOLON,
    COLON,
    COLONCOLON,
    DOT,
    DOTDOT,
    QUEST,
    QUESTDOT,
    AS_SAFE,
    /// `!is` — the negated type-check operator, fused from `!` + `is`.
    NOT_IS,
    /// `!in` — the negated membership operator, fused from `!` + `in`.
    NOT_IN,
    AT,
    ARROW,
    EQ,
    EQEQ,
    EQEQEQ,
    EXCLEQ,
    EXCLEQEQ,
    EXCL,
    EXCLEXCL,
    LT,
    GT,
    LTEQ,
    GTEQ,
    ANDAND,
    OROR,
    PLUS,
    MINUS,
    MUL,
    DIV,
    PERC,
    PLUSEQ,
    MINUSEQ,
    MULEQ,
    DIVEQ,
    PERCEQ,
    PLUSPLUS,
    MINUSMINUS,
    ELVIS,
    HASH, // not currently lexed, but kotlinc PSI references it

    // trivia (only emitted when the lexer runs with `preserve_trivia`)
    WHITE_SPACE,
    LINE_COMMENT,
    BLOCK_COMMENT,
    // structural
    NEWLINE,
    LEX_ERROR,

    // ─── KDoc ───────────────────────────────────────────────────────────
    KDOC,
    KDOC_START,
    KDOC_END,
    KDOC_LEADING_ASTERISK,
    KDOC_SECTION,
    KDOC_TAG,
    KDOC_TAG_NAME,
    KDOC_NAME,
    KDOC_TEXT,
    KDOC_CODE_BLOCK_TEXT,
    KDOC_LPAR,
    KDOC_RPAR,
    KDOC_MARKDOWN_LINK,
    KDOC_MARKDOWN_INLINE_LINK,
}

impl SyntaxKind {
    /// `true` when the kind represents a leaf in the syntax tree — a
    /// token, trivia, or one of the special markers. Returns `false`
    /// for composites that have child nodes.
    pub fn is_token(self) -> bool {
        // Anything at or above IDENTIFIER in the enum is a leaf. Keeping
        // this as an explicit range rather than `matches!` so adding a
        // new composite at the top of the enum doesn't silently
        // reclassify existing tokens.
        (self as u16) >= (SyntaxKind::IDENTIFIER as u16)
            && (self as u16) <= (SyntaxKind::LEX_ERROR as u16)
    }

    /// Inverse of [`Self::is_token`], minus the `TOMBSTONE`/`EOF`/`ERROR_ELEMENT`
    /// markers and KDoc kinds (those are categorized separately).
    pub fn is_composite(self) -> bool {
        let n = self as u16;
        n >= (SyntaxKind::FILE as u16) && n <= (SyntaxKind::NULL_CONSTANT as u16)
    }

    /// `true` for tokens the FIR parser would have silently consumed:
    /// whitespace runs and any comment flavor. The SIL pipeline still
    /// emits these into its tree so the source can be reconstructed
    /// byte-for-byte.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::WHITE_SPACE | SyntaxKind::LINE_COMMENT | SyntaxKind::BLOCK_COMMENT
        )
    }

    /// `true` for any KDoc-related kind (the root `KDOC` node, the
    /// sub-tree it contains, and the structural leaf tokens emitted by
    /// the KDoc sub-parser).
    pub fn is_kdoc(self) -> bool {
        let n = self as u16;
        n >= (SyntaxKind::KDOC as u16) && n <= (SyntaxKind::KDOC_MARKDOWN_INLINE_LINK as u16)
    }

    /// Project a [`TokenKind`] (from the lexer) into the matching
    /// [`SyntaxKind`] (for the parser's event stream). Total — every
    /// `TokenKind` variant has a corresponding `SyntaxKind`.
    pub fn from_token_kind(t: TokenKind) -> Self {
        match t {
            // identifiers and literals
            TokenKind::Ident => SyntaxKind::IDENTIFIER,
            TokenKind::IntLit => SyntaxKind::INTEGER_LITERAL,
            TokenKind::CharLit => SyntaxKind::CHARACTER_LITERAL,
            TokenKind::LongLit => SyntaxKind::LONG_LITERAL,
            TokenKind::DoubleLit => SyntaxKind::DOUBLE_LITERAL,
            TokenKind::FloatLit => SyntaxKind::FLOAT_LITERAL,
            TokenKind::StringLit => SyntaxKind::STRING_LITERAL,
            // string-template state machine
            TokenKind::StringStart => SyntaxKind::STRING_START,
            TokenKind::StringChunk => SyntaxKind::STRING_CHUNK,
            TokenKind::StringIdentRef => SyntaxKind::STRING_IDENT_REF,
            TokenKind::StringExprStart => SyntaxKind::STRING_EXPR_START,
            TokenKind::StringExprEnd => SyntaxKind::STRING_EXPR_END,
            TokenKind::StringEnd => SyntaxKind::STRING_END,
            // keywords
            TokenKind::KwFun => SyntaxKind::KW_FUN,
            TokenKind::KwVal => SyntaxKind::KW_VAL,
            TokenKind::KwVar => SyntaxKind::KW_VAR,
            TokenKind::KwIf => SyntaxKind::KW_IF,
            TokenKind::KwElse => SyntaxKind::KW_ELSE,
            TokenKind::KwReturn => SyntaxKind::KW_RETURN,
            TokenKind::KwTrue => SyntaxKind::KW_TRUE,
            TokenKind::KwFalse => SyntaxKind::KW_FALSE,
            TokenKind::KwNull => SyntaxKind::KW_NULL,
            TokenKind::KwWhile => SyntaxKind::KW_WHILE,
            TokenKind::KwDo => SyntaxKind::KW_DO,
            TokenKind::KwWhen => SyntaxKind::KW_WHEN,
            TokenKind::KwFor => SyntaxKind::KW_FOR,
            TokenKind::KwIn => SyntaxKind::KW_IN,
            TokenKind::KwBreak => SyntaxKind::KW_BREAK,
            TokenKind::KwContinue => SyntaxKind::KW_CONTINUE,
            TokenKind::KwClass => SyntaxKind::KW_CLASS,
            TokenKind::KwObject => SyntaxKind::KW_OBJECT,
            TokenKind::KwPackage => SyntaxKind::KW_PACKAGE,
            TokenKind::KwImport => SyntaxKind::KW_IMPORT,
            TokenKind::KwConst => SyntaxKind::KW_CONST,
            TokenKind::KwThrow => SyntaxKind::KW_THROW,
            TokenKind::KwTry => SyntaxKind::KW_TRY,
            TokenKind::KwCatch => SyntaxKind::KW_CATCH,
            TokenKind::KwFinally => SyntaxKind::KW_FINALLY,
            TokenKind::KwIs => SyntaxKind::KW_IS,
            TokenKind::KwAs => SyntaxKind::KW_AS,
            TokenKind::KwSuper => SyntaxKind::KW_SUPER,
            TokenKind::KwInit => SyntaxKind::KW_INIT,
            TokenKind::KwData => SyntaxKind::KW_DATA,
            TokenKind::KwEnum => SyntaxKind::KW_ENUM,
            TokenKind::KwInterface => SyntaxKind::KW_INTERFACE,
            TokenKind::KwSealed => SyntaxKind::KW_SEALED,
            TokenKind::KwOverride => SyntaxKind::KW_OVERRIDE,
            TokenKind::KwOpen => SyntaxKind::KW_OPEN,
            TokenKind::KwAbstract => SyntaxKind::KW_ABSTRACT,
            TokenKind::KwInfix => SyntaxKind::KW_INFIX,
            TokenKind::KwInline => SyntaxKind::KW_INLINE,
            TokenKind::KwPrivate => SyntaxKind::KW_PRIVATE,
            TokenKind::KwProtected => SyntaxKind::KW_PROTECTED,
            TokenKind::KwInternal => SyntaxKind::KW_INTERNAL,
            TokenKind::KwOperator => SyntaxKind::KW_OPERATOR,
            TokenKind::KwVararg => SyntaxKind::KW_VARARG,
            TokenKind::KwConstructor => SyntaxKind::KW_CONSTRUCTOR,
            TokenKind::KwLateinit => SyntaxKind::KW_LATEINIT,
            TokenKind::KwSuspend => SyntaxKind::KW_SUSPEND,
            TokenKind::KwTailrec => SyntaxKind::KW_TAILREC,
            // punctuation
            TokenKind::LParen => SyntaxKind::LPAR,
            TokenKind::RParen => SyntaxKind::RPAR,
            TokenKind::LBrace => SyntaxKind::LBRACE,
            TokenKind::RBrace => SyntaxKind::RBRACE,
            TokenKind::LBracket => SyntaxKind::LBRACKET,
            TokenKind::RBracket => SyntaxKind::RBRACKET,
            TokenKind::Comma => SyntaxKind::COMMA,
            TokenKind::Semi => SyntaxKind::SEMICOLON,
            TokenKind::Colon => SyntaxKind::COLON,
            TokenKind::ColonColon => SyntaxKind::COLONCOLON,
            TokenKind::Dot => SyntaxKind::DOT,
            TokenKind::DotDot => SyntaxKind::DOTDOT,
            TokenKind::Question => SyntaxKind::QUEST,
            TokenKind::QuestionDot => SyntaxKind::QUESTDOT,
            TokenKind::At => SyntaxKind::AT,
            TokenKind::Arrow => SyntaxKind::ARROW,
            TokenKind::Eq => SyntaxKind::EQ,
            TokenKind::EqEq => SyntaxKind::EQEQ,
            TokenKind::EqEqEq => SyntaxKind::EQEQEQ,
            TokenKind::NotEq => SyntaxKind::EXCLEQ,
            TokenKind::NotEqEq => SyntaxKind::EXCLEQEQ,
            TokenKind::Bang => SyntaxKind::EXCL,
            TokenKind::BangBang => SyntaxKind::EXCLEXCL,
            TokenKind::Lt => SyntaxKind::LT,
            TokenKind::Gt => SyntaxKind::GT,
            TokenKind::LtEq => SyntaxKind::LTEQ,
            TokenKind::GtEq => SyntaxKind::GTEQ,
            TokenKind::AmpAmp => SyntaxKind::ANDAND,
            TokenKind::PipePipe => SyntaxKind::OROR,
            TokenKind::Plus => SyntaxKind::PLUS,
            TokenKind::Minus => SyntaxKind::MINUS,
            TokenKind::Star => SyntaxKind::MUL,
            TokenKind::Slash => SyntaxKind::DIV,
            TokenKind::Percent => SyntaxKind::PERC,
            TokenKind::PlusEq => SyntaxKind::PLUSEQ,
            TokenKind::MinusEq => SyntaxKind::MINUSEQ,
            TokenKind::StarEq => SyntaxKind::MULEQ,
            TokenKind::SlashEq => SyntaxKind::DIVEQ,
            TokenKind::PercentEq => SyntaxKind::PERCEQ,
            TokenKind::PlusPlus => SyntaxKind::PLUSPLUS,
            TokenKind::MinusMinus => SyntaxKind::MINUSMINUS,
            TokenKind::Elvis => SyntaxKind::ELVIS,
            // trivia / structural
            TokenKind::Newline => SyntaxKind::NEWLINE,
            TokenKind::Whitespace => SyntaxKind::WHITE_SPACE,
            TokenKind::LineComment => SyntaxKind::LINE_COMMENT,
            TokenKind::BlockComment => SyntaxKind::BLOCK_COMMENT,
            TokenKind::DocComment => SyntaxKind::KDOC,
            TokenKind::Eof => SyntaxKind::EOF,
            TokenKind::Error => SyntaxKind::LEX_ERROR,
        }
    }
}

impl From<TokenKind> for SyntaxKind {
    fn from(t: TokenKind) -> Self {
        SyntaxKind::from_token_kind(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_kinds_classify_as_tokens() {
        assert!(SyntaxKind::IDENTIFIER.is_token());
        assert!(SyntaxKind::KW_FUN.is_token());
        assert!(SyntaxKind::LPAR.is_token());
        assert!(SyntaxKind::WHITE_SPACE.is_token());
        assert!(SyntaxKind::NEWLINE.is_token());
    }

    #[test]
    fn composite_kinds_classify_as_composites() {
        assert!(SyntaxKind::FILE.is_composite());
        assert!(SyntaxKind::FUN.is_composite());
        assert!(SyntaxKind::BINARY_EXPRESSION.is_composite());
        assert!(!SyntaxKind::FUN.is_token());
        assert!(!SyntaxKind::IDENTIFIER.is_composite());
    }

    #[test]
    fn trivia_classifies_as_trivia() {
        assert!(SyntaxKind::WHITE_SPACE.is_trivia());
        assert!(SyntaxKind::LINE_COMMENT.is_trivia());
        assert!(SyntaxKind::BLOCK_COMMENT.is_trivia());
        // NEWLINE is structural in Kotlin (newline-sensitive grammar),
        // not trivia.
        assert!(!SyntaxKind::NEWLINE.is_trivia());
        // KDoc is its own category — neither plain trivia nor composite
        // for the FIR parser's purposes.
        assert!(!SyntaxKind::KDOC.is_trivia());
        assert!(SyntaxKind::KDOC.is_kdoc());
    }

    #[test]
    fn from_token_kind_covers_every_variant() {
        // Smoke test: every TokenKind round-trips into some SyntaxKind
        // (the actual match is total; this just guards the conversion
        // against silent panics if a new TokenKind is added without an
        // arm).
        for tk in [
            TokenKind::Ident,
            TokenKind::IntLit,
            TokenKind::KwFun,
            TokenKind::LParen,
            TokenKind::Eof,
            TokenKind::Whitespace,
            TokenKind::DocComment,
        ] {
            let _ = SyntaxKind::from_token_kind(tk);
        }
    }
}
