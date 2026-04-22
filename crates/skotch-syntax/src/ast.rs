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

/// Kotlin visibility modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
    Internal,
}

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
    /// `object Singleton { fun greet() { } }` — singleton declaration.
    Object(ObjectDecl),
    /// `enum class Color { RED, GREEN, BLUE }` — enum declaration.
    Enum(EnumDecl),
    /// `interface Printable { fun prettyPrint(): String }` — interface declaration.
    Interface(InterfaceDecl),
    /// `typealias Name = UnderlyingType`.
    TypeAlias(TypeAliasDecl),
    /// Recognised but not implemented.
    Unsupported {
        what: &'static str,
        span: Span,
    },
}

/// A type alias declaration: `typealias StringList = List<String>`.
#[derive(Clone, Debug)]
pub struct TypeAliasDecl {
    pub name: Symbol,
    pub name_span: Span,
    /// Type parameters: `typealias Predicate<T> = (T) -> Boolean`.
    pub type_params: Vec<TypeParam>,
    /// The underlying type this alias resolves to.
    pub target: TypeRef,
    pub span: Span,
}

/// An interface declaration.
#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    pub name: Symbol,
    pub name_span: Span,
    /// Methods declared in the interface body (abstract by default).
    pub methods: Vec<FunDecl>,
    pub span: Span,
}

/// An enum class declaration.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub name: Symbol,
    pub name_span: Span,
    /// Constructor params: `enum class Color(val hex: Int)`.
    pub constructor_params: Vec<ConstructorParam>,
    /// Enum constant names with optional args in declaration order.
    pub entries: Vec<EnumEntry>,
    /// Methods declared on the enum class body (after the entries).
    /// Includes abstract methods that entries must override.
    pub methods: Vec<FunDecl>,
    pub span: Span,
}

/// An enum constant entry, optionally with constructor arguments.
#[derive(Clone, Debug)]
pub struct EnumEntry {
    pub name: Symbol,
    pub args: Vec<Expr>,
    /// Methods overridden in this entry's anonymous class body.
    pub methods: Vec<FunDecl>,
}

/// An `object` declaration (singleton).
#[derive(Clone, Debug)]
pub struct ObjectDecl {
    pub name: Symbol,
    pub name_span: Span,
    /// Methods declared in the object body.
    pub methods: Vec<FunDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FunDecl {
    pub name: Symbol,
    pub name_span: Span,
    /// Type parameters: `fun <T, R> map(...)`.
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub return_ty: Option<TypeRef>,
    /// For extension functions: the receiver type (e.g. `String` in `fun String.exclaim()`).
    /// The receiver becomes accessible as `this` in the function body.
    pub receiver_ty: Option<TypeRef>,
    pub body: Block,
    pub is_open: bool,
    pub is_override: bool,
    pub is_abstract: bool,
    /// True when declared with the `suspend` modifier.
    pub is_suspend: bool,
    pub visibility: Visibility,
    pub span: Span,
}

/// A type parameter declaration: `T`, `T : Comparable<T>`, `out T`, `in T`.
#[derive(Clone, Debug)]
pub struct TypeParam {
    pub name: Symbol,
    /// Upper bound: `T : Comparable<T>` → bound is `Comparable`.
    pub bound: Option<Symbol>,
    /// `reified T` — type is available at runtime (inline functions only).
    pub is_reified: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Symbol,
    pub ty: TypeRef,
    /// Default value expression, e.g. `fun greet(name: String = "world")`.
    pub default: Option<Box<Expr>>,
    /// True when declared as `vararg numbers: Int`.
    pub is_vararg: bool,
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
    pub is_open: bool,
    pub is_abstract: bool,
    pub name: Symbol,
    pub name_span: Span,
    /// Type parameters: `class Box<T>(...)`.
    pub type_params: Vec<TypeParam>,
    /// Primary constructor parameters (may include `val`/`var` properties).
    pub constructor_params: Vec<ConstructorParam>,
    /// Superclass clause: `: ParentClass(args)`.
    pub parent_class: Option<SuperClassRef>,
    /// Implemented interfaces (from `: Interface1, Interface2` after superclass).
    pub interfaces: Vec<Symbol>,
    /// Interface delegation: `class Derived(b: Base) : Base by b`.
    /// Each entry is `(interface_name, delegate_param_name)`.
    pub interface_delegates: Vec<(Symbol, Symbol)>,
    /// Properties declared in the class body.
    pub properties: Vec<PropertyDecl>,
    /// Methods declared in the class body.
    pub methods: Vec<FunDecl>,
    /// Methods from a `companion object { }` block.
    pub companion_methods: Vec<FunDecl>,
    /// Properties from a `companion object { }` block.
    pub companion_properties: Vec<PropertyDecl>,
    /// Init blocks (statements run in the constructor).
    pub init_blocks: Vec<Block>,
    /// Secondary constructors: `constructor(params) : this(args) { body }`.
    pub secondary_constructors: Vec<SecondaryConstructor>,
    /// Nested (static inner) classes: `class Outer { class Nested { } }`.
    pub nested_classes: Vec<ClassDecl>,
    /// `inner class` flag — inner classes hold a reference to the outer instance.
    pub is_inner: bool,
    pub span: Span,
}

/// A secondary constructor declared in the class body.
#[derive(Clone, Debug)]
pub struct SecondaryConstructor {
    pub params: Vec<Param>,
    /// True when the constructor has an explicit `: this(args)` delegation.
    pub has_delegation: bool,
    /// Arguments passed to the delegate constructor call (`this(args)`).
    pub delegate_args: Vec<Expr>,
    /// Optional body block.
    pub body: Option<Block>,
    pub span: Span,
}

/// Reference to a superclass in a class declaration: `ClassName(args)`.
#[derive(Clone, Debug)]
pub struct SuperClassRef {
    pub name: Symbol,
    pub name_span: Span,
    pub args: Vec<CallArg>,
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
    /// `lateinit var` — no initializer required; field starts as null on JVM.
    pub is_lateinit: bool,
    pub name: Symbol,
    pub name_span: Span,
    pub ty: Option<TypeRef>,
    pub init: Option<Expr>,
    /// Delegate expression: `val x by lazy { ... }`.
    /// Stores the body of the `lazy` lambda for eager desugaring.
    pub delegate: Option<Box<Block>>,
    /// Custom getter: `val x: Int get() = expr`.
    pub getter: Option<Block>,
    /// Custom setter: `var x: Int set(value) { ... }`.
    pub setter: Option<(Symbol, Block)>,
    pub span: Span,
}

/// An argument in a function call, optionally named.
#[derive(Clone, Debug)]
pub struct CallArg {
    /// If `Some`, this is a named argument: `name = expr`.
    pub name: Option<Symbol>,
    pub expr: Expr,
}

/// A surface-level type reference.
#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: Symbol,
    pub nullable: bool,
    /// For function types: `(Int, String) -> Boolean`.
    /// `func_params` holds the parameter types; `name` holds the return type.
    pub func_params: Option<Vec<TypeRef>>,
    /// Generic type arguments: `List<Int>`, `Map<String, Int>`.
    pub type_args: Vec<TypeRef>,
    /// True when this function type is declared `suspend`, e.g.
    /// `suspend () -> String`.  Non-function TypeRefs always have
    /// this set to `false`.
    pub is_suspend: bool,
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
    /// `return [expr]` or `return@label [expr]`.
    Return {
        value: Option<Expr>,
        /// Labeled return: `return@forEach` exits the lambda, not the
        /// enclosing function.
        label: Option<Symbol>,
        span: Span,
    },
    /// Local function declaration inside a block.
    LocalFun(FunDecl),
    /// `break` or `break@label` — exits a loop.
    Break { label: Option<Symbol>, span: Span },
    /// `continue` or `continue@label` — skips to next iteration.
    Continue { label: Option<Symbol>, span: Span },
    /// `while (cond) { body }`.
    While { cond: Expr, body: Block, span: Span },
    /// `do { body } while (cond)`.
    DoWhile { body: Block, cond: Expr, span: Span },
    /// `for (name in start..end) { body }` or `for (name in start until end) { body }`.
    /// Optional `step` for `for (i in 0..10 step 2)`.
    For {
        var_name: Symbol,
        start: Expr,
        end: Expr,
        exclusive: bool,
        descending: bool,
        /// Optional step expression: `for (i in 1..10 step 2)`.
        step: Option<Expr>,
        body: Block,
        span: Span,
    },
    /// `for (name in collection) { body }` — collection iteration.
    ForIn {
        var_name: Symbol,
        iterable: Expr,
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
    /// `receiver[index] = value` — array/collection index assignment.
    IndexAssign {
        receiver: Expr,
        index: Expr,
        value: Expr,
        span: Span,
    },
    /// `receiver.field = value` — field/property assignment.
    FieldAssign {
        receiver: Expr,
        field: Symbol,
        value: Expr,
        span: Span,
    },
    /// `val (a, b, c) = expr` — destructuring declaration.
    /// Desugars to `val a = expr.component1()`, `val b = expr.component2()`, etc.
    Destructure {
        names: Vec<Symbol>,
        init: Expr,
        span: Span,
    },
}

/// Expressions exercised by PR #1 fixtures.
#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64, Span),
    /// Character literal: `'a'`, `'\n'`. Stores the code point.
    CharLit(i64, Span),
    LongLit(i64, Span),
    DoubleLit(f64, Span),
    /// Float literal with `f`/`F` suffix: `3.14f`.
    FloatLit(f64, Span),
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
        /// Explicit type arguments: `f<String>(arg)`.
        type_args: Vec<TypeRef>,
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
    /// `{ x: Int -> x * 2 }` — lambda expression.
    Lambda {
        params: Vec<Param>,
        body: Block,
        /// `true` if this lambda is declared `suspend` (e.g. via the
        /// `suspend` keyword on the containing function-type parameter,
        /// or when the body contains suspend calls). Suspend lambdas
        /// compile to a `kotlin/coroutines/jvm/internal/SuspendLambda`
        /// subclass instead of a regular `$Lambda$N` class.
        ///
        /// **Session 6 scope:** flag is carried through the AST and MIR
        /// but full SuspendLambda codegen (SuspendLambda superclass,
        /// invokeSuspend state machine, create/invoke/bridge methods)
        /// is not yet emitted. Lambdas with suspend bodies currently
        /// fall back to the regular $Lambda$N class, which means they
        /// won't work at runtime when passed to coroutine builders.
        is_suspend: bool,
        span: Span,
    },
    /// `object : InterfaceName { override fun method() { } }` — anonymous object.
    ObjectExpr {
        /// The interface or class being implemented.
        super_type: Symbol,
        /// Methods declared in the object body.
        methods: Vec<FunDecl>,
        span: Span,
    },
    /// `receiver[index]` — array/collection indexing.
    Index {
        receiver: Box<Expr>,
        index: Box<Expr>,
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
            | Expr::CharLit(_, s)
            | Expr::LongLit(_, s)
            | Expr::DoubleLit(_, s)
            | Expr::FloatLit(_, s)
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
            | Expr::NotNullAssert { span, .. }
            | Expr::Lambda { span, .. }
            | Expr::ObjectExpr { span, .. }
            | Expr::Index { span, .. } => *span,
        }
    }
}
