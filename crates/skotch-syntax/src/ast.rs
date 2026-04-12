//! AST node types for Kotlin source files.
//!
//! Trees are simple `Box`-based, not `rowan`. Rowan would give us a
//! lossless syntax tree (whitespace + comments preserved) which is
//! valuable for an LSP, but it adds substantial boilerplate before the
//! parser produces its first node. Box-trees suffice for the PR #1
//! goal of "compile to .class". When the LSP becomes a goal we'll
//! revisit.
//!
//! ## Coverage
//!
//! Only the syntax exercised by the PR #1 fixtures (01–10) is modeled
//! here. Future fixtures (classes, when, lambdas, generics, ...) will
//! grow this file. The parser already accepts and rejects shapes more
//! liberally than this AST encodes — see `skotch-parser` — so we can
//! report a friendly "not yet supported" message rather than a syntax
//! error for features the user reasonably expects to work.

use skotch_intern::Symbol;
use skotch_span::{FileId, Span};

/// One parsed `.kt` file.
#[derive(Clone, Debug)]
pub struct KtFile {
    pub file: FileId,
    pub package: Option<PackageDecl>,
    pub imports: Vec<ImportDecl>,
    pub decls: Vec<Decl>,
}

#[derive(Clone, Debug)]
pub struct PackageDecl {
    pub path: Vec<Symbol>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: Vec<Symbol>,
    /// True for `import foo.bar.*` (star import).
    pub is_wildcard: bool,
    pub span: Span,
}

/// A top-level declaration. Class/object/etc. live as `Unsupported` for
/// PR #1 — the parser collects them so it can produce a useful diagnostic
/// rather than a syntax error.
#[derive(Clone, Debug)]
pub enum Decl {
    Fun(FunDecl),
    Val(ValDecl),
    Class(ClassDecl),
    /// Recognised but not implemented. The parser may attach this for
    /// e.g. `object Foo`. Carries the original span for diagnostics.
    Unsupported {
        what: &'static str,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct FunDecl {
    pub name: Symbol,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub return_ty: Option<TypeRef>,
    /// For extension functions: the receiver type (e.g. `String` in `fun String.exclaim()`).
    /// The receiver becomes accessible as `this` in the function body.
    pub receiver_ty: Option<TypeRef>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Symbol,
    pub ty: TypeRef,
    /// Default value expression, e.g. `fun greet(name: String = "world")`.
    pub default: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ValDecl {
    pub is_var: bool,
    pub name: Symbol,
    pub name_span: Span,
    pub ty: Option<TypeRef>,
    pub init: Expr,
    pub span: Span,
}

/// A class declaration.
#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub is_data: bool,
    pub name: Symbol,
    pub name_span: Span,
    /// Primary constructor parameters (may include `val`/`var` properties).
    pub constructor_params: Vec<ConstructorParam>,
    /// Properties declared in the class body.
    pub properties: Vec<PropertyDecl>,
    /// Methods declared in the class body.
    pub methods: Vec<FunDecl>,
    /// Init blocks (statements run in the constructor).
    pub init_blocks: Vec<Block>,
    pub span: Span,
}

/// A primary constructor parameter, optionally a property (`val`/`var`).
#[derive(Clone, Debug)]
pub struct ConstructorParam {
    pub is_val: bool,
    pub is_var: bool,
    pub name: Symbol,
    pub ty: TypeRef,
    pub span: Span,
}

/// A property declaration inside a class body.
#[derive(Clone, Debug)]
pub struct PropertyDecl {
    pub is_var: bool,
    pub name: Symbol,
    pub name_span: Span,
    pub ty: Option<TypeRef>,
    pub init: Option<Expr>,
    pub span: Span,
}

/// An argument in a function call, optionally named.
#[derive(Clone, Debug)]
pub struct CallArg {
    /// If `Some`, this is a named argument: `name = expr`.
    pub name: Option<Symbol>,
    pub expr: Expr,
}

/// A surface-level type reference. We don't model generics yet.
#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: Symbol,
    pub nullable: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// A bare expression used as a statement.
    Expr(Expr),
    /// A local `val` or `var` declaration.
    Val(ValDecl),
    /// `return [expr]`.
    Return { value: Option<Expr>, span: Span },
    /// Local function declaration inside a block.
    LocalFun(FunDecl),
    /// `break` — exits the innermost loop.
    Break(Span),
    /// `continue` — skips to the next iteration of the innermost loop.
    Continue(Span),
    /// `while (cond) { body }`.
    While { cond: Expr, body: Block, span: Span },
    /// `do { body } while (cond)`.
    DoWhile { body: Block, cond: Expr, span: Span },
    /// `for (name in start..end) { body }` or `for (name in start until end) { body }`.
    For {
        var_name: Symbol,
        start: Expr,
        end: Expr,
        /// If true, use exclusive end (`until`); if false, inclusive (`..`).
        exclusive: bool,
        body: Block,
        span: Span,
    },
    /// `lhs = rhs` reassignment (for `var` targets).
    Assign {
        target: Symbol,
        value: Expr,
        span: Span,
    },
    /// `try { body } catch (e: Type) { handler } finally { cleanup }`
    TryStmt {
        body: Block,
        catch_param: Option<Symbol>,
        catch_type: Option<Symbol>,
        catch_body: Option<Block>,
        finally_body: Option<Block>,
        span: Span,
    },
    /// `throw expr`
    ThrowStmt { expr: Expr, span: Span },
}

/// Expressions exercised by PR #1 fixtures.
#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64, Span),
    LongLit(i64, Span),
    DoubleLit(f64, Span),
    BoolLit(bool, Span),
    NullLit(Span),
    StringLit(String, Span),
    /// A string literal with interpolation. Each part is either literal
    /// text or an embedded expression.
    StringTemplate(Vec<TemplatePart>, Span),
    Ident(Symbol, Span),
    /// `f(arg, arg)` or `obj.f(arg)`. Supports named arguments:
    /// `f(name = value, other = value)`.
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Paren(Box<Expr>, Span),
    /// `if (cond) then-block else else-block`. Either branch may be a
    /// single-expression branch (we wrap it in a `Block` of one stmt).
    /// `else` is optional; an `if` used as an expression must have it.
    If {
        cond: Box<Expr>,
        then_block: Box<Block>,
        else_block: Option<Box<Block>>,
        span: Span,
    },
    /// `when (subject) { value -> expr, ..., else -> expr }`.
    When {
        subject: Box<Expr>,
        branches: Vec<WhenBranch>,
        else_body: Option<Box<Expr>>,
        span: Span,
    },
    /// `obj.field` or `obj.method` (the latter is wrapped in a `Call`).
    Field {
        receiver: Box<Expr>,
        name: Symbol,
        span: Span,
    },
    /// `throw expr` — throws an exception.
    Throw {
        expr: Box<Expr>,
        span: Span,
    },
    /// `try { body } catch (e: Type) { handler } finally { cleanup }`
    Try {
        body: Box<Block>,
        catch_param: Option<Symbol>,
        catch_type: Option<Symbol>,
        catch_body: Option<Box<Block>>,
        finally_body: Option<Box<Block>>,
        span: Span,
    },
    /// `lhs ?: rhs` — elvis operator.
    ElvisOp {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// `expr?.field` or `expr?.method()` — safe call.
    SafeCall {
        receiver: Box<Expr>,
        name: Symbol,
        span: Span,
    },
    /// `expr is Type` — type check.
    IsCheck {
        expr: Box<Expr>,
        type_name: Symbol,
        negated: bool,
        span: Span,
    },
    /// `expr as Type` — type cast.
    AsCast {
        expr: Box<Expr>,
        type_name: Symbol,
        safe: bool, // `as?`
        span: Span,
    },
    /// `expr!!` — non-null assertion.
    NotNullAssert {
        expr: Box<Expr>,
        span: Span,
    },
}

/// A single branch in a `when` expression: `pattern -> body`.
#[derive(Clone, Debug)]
pub struct WhenBranch {
    pub pattern: Expr,
    /// For `in start..end` patterns: the end of the range.
    /// When `Some`, this is a range check: `subject in pattern..range_end`.
    pub range_end: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

/// One piece of an interpolated string.
#[derive(Clone, Debug)]
pub enum TemplatePart {
    /// A literal text run between interpolations.
    Text(String, Span),
    /// `$identifier`.
    IdentRef(Symbol, Span),
    /// `${ expression }`.
    Expr(Expr),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Not,
}

impl Expr {
    /// Source span of this expression. Used by diagnostics.
    pub fn span(&self) -> Span {
        match self {
            Expr::IntLit(_, s)
            | Expr::LongLit(_, s)
            | Expr::DoubleLit(_, s)
            | Expr::BoolLit(_, s)
            | Expr::NullLit(s)
            | Expr::StringLit(_, s)
            | Expr::StringTemplate(_, s)
            | Expr::Ident(_, s)
            | Expr::Paren(_, s) => *s,
            Expr::Call { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::If { span, .. }
            | Expr::When { span, .. }
            | Expr::Field { span, .. }
            | Expr::Throw { span, .. }
            | Expr::Try { span, .. }
            | Expr::ElvisOp { span, .. }
            | Expr::SafeCall { span, .. }
            | Expr::IsCheck { span, .. }
            | Expr::AsCast { span, .. }
            | Expr::NotNullAssert { span, .. } => *span,
        }
    }
}
