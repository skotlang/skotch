//! Name resolution for the Kotlin subset skotch accepts.
//!
//! Walks a [`KtFile`] AST, builds a flat scope tree, and resolves
//! identifier references to [`DefId`]s. Currently supports:
//!
//! - top-level `fun` declarations as static methods
//! - top-level `val` declarations as static final fields
//! - local `val`/`var` declarations
//! - the hard-coded built-in `println` (mapped to `DefId::PrintlnIntrinsic`)
//! - parameter references inside function bodies
//!
//! No imports, no classes, no method receivers — those produce a
//! "not yet supported" diagnostic and an [`DefId::Error`] reference.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_span::{FileId, Span};
use skotch_syntax::{
    Annotation, Block, ClassDecl, ConstructorParam, Decl, Expr, FunDecl, KtFile, Param, Stmt,
    TemplatePart, TypeRef, ValDecl, Visibility,
};
use skotch_types::Ty;

pub mod typed;

/// Stable identifier for any *defined* thing the resolver knows about.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DefId {
    /// Top-level function. Index into [`ResolvedFile::functions`].
    Function(u32),
    /// Top-level value (lowered to a static final field).
    TopLevelVal(u32),
    /// Local val/var inside a function. The pair is (function index,
    /// local slot).
    Local(u32, u32),
    /// Function parameter. Pair is (function index, parameter index).
    Param(u32, u32),
    /// The built-in `println` intrinsic — accepts any single argument.
    PrintlnIntrinsic,
    /// An identifier used as a method-call receiver that isn't in local
    /// scope or top-level declarations. Deferred to MIR lowering for
    /// resolution against the Java class registry. Carries the Symbol
    /// so the name can be looked up later.
    PossibleExternal(Symbol),
    /// Declaration from another file in the same package/module.
    /// Deferred to MIR lowering, which has the PackageSymbolTable
    /// with JVM class/descriptor details.
    ExternalPackage(Symbol),
    /// Marker for an unresolved reference; the resolver has already
    /// emitted a diagnostic and downstream passes should stop.
    Error,
}

#[derive(Clone, Debug)]
pub struct ResolvedFunction {
    pub name: Symbol,
    pub params: Vec<Symbol>,
    /// Local slot table built during resolution. Indexed by local slot;
    /// each entry is the source span of the original declaration.
    pub locals: Vec<Symbol>,
    pub body_refs: Vec<ResolvedRef>,
}

#[derive(Clone, Debug)]
pub struct ResolvedTopLevelVal {
    pub name: Symbol,
    pub init_refs: Vec<ResolvedRef>,
}

/// One resolved identifier or call. The MIR-lowering pass walks the AST
/// alongside this side table.
#[derive(Clone, Debug)]
pub struct ResolvedRef {
    pub span: Span,
    pub def: DefId,
}

#[derive(Default, Clone, Debug)]
pub struct ResolvedFile {
    pub functions: Vec<ResolvedFunction>,
    pub top_vals: Vec<ResolvedTopLevelVal>,
    /// Lookup from a top-level `Symbol` (function or val name) to its
    /// `DefId`. Used by the typeck and lowering passes when they walk
    /// call expressions.
    pub top_level: FxHashMap<Symbol, DefId>,
}

// ── Multi-file compilation support ──────────────────────────────────

/// Top-level declarations visible across files within a compilation unit.
/// Built by [`gather_declarations`] from all parsed KtFiles before
/// resolution, enabling cross-file function calls and class references.
///
/// Uses `HashMap` (not `FxHashMap`) so the struct can be serialized/
/// deserialized by serde for Salsa incremental compilation.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PackageSymbolTable {
    /// Top-level function: name → declaration metadata (may have overloads).
    pub functions: std::collections::HashMap<String, Vec<ExternalFunDecl>>,
    /// Top-level val: name → declaration metadata.
    pub vals: std::collections::HashMap<String, ExternalValDecl>,
    /// User-defined class/object/enum/interface: simple name → declaration
    /// metadata. The lookup shape most consumers want — an AST `Ident`
    /// referring to `Foo` finds the entry for the class named `Foo`.
    /// When two classes in different sub-packages share a simple name,
    /// the second insertion wins this map; `classes_by_fq` keeps both.
    pub classes: std::collections::HashMap<String, ExternalClassDecl>,
    /// FQ-name index over the same `ExternalClassDecl` instances. Use
    /// this when you already have a JVM-internal name in hand and want
    /// the corresponding decl without going through the simple-name
    /// lookup (which can drop entries on collision).
    #[serde(default)]
    pub classes_by_fq: std::collections::HashMap<String, ExternalClassDecl>,
    /// `typealias` declarations: simple-name → AST shape of the alias.
    /// Surfaced so typeck in another file can resolve the alias to its
    /// underlying type, with the alias's own type parameters bound to
    /// the actual type args at the use site.
    ///
    /// Not serialized — `TypeRef`/`TypeParam` don't impl serde and the
    /// table rebuilds each compile.
    #[serde(skip)]
    pub type_aliases: std::collections::HashMap<String, ExternalTypeAlias>,
    /// Secondary index from simple class name → FQ class name. The
    /// primary `classes` map is keyed by FQ name to disambiguate
    /// classes with the same simple name in different sub-packages.
    /// Most call sites look up by simple name and resolve through this
    /// index. Collisions emit a diagnostic from `gather_declarations`.
    #[serde(default)]
    pub simple_name_to_fq: std::collections::HashMap<String, String>,
}

/// AST-shaped metadata for a cross-file typealias. Carries enough to
/// resolve `Predicate<Int>` (typealias `Predicate<T> = (T) -> Boolean`)
/// into `(Int) -> Boolean` at the use site, not just the bare
/// `(T) -> Boolean` shape.
#[derive(Clone, Debug)]
pub struct ExternalTypeAlias {
    pub type_params: Vec<skotch_syntax::TypeParam>,
    pub target: skotch_syntax::TypeRef,
}

/// A single parameter on a cross-file function or constructor —
/// name + type + whether the source declaration provided a default
/// value. The `has_default` flag drives the JVM `$default(... I)`
/// thunk dispatch so call sites that omit defaultable args pick the
/// right descriptor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalParam {
    pub name: String,
    pub ty: Ty,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_default: bool,
    /// True when the source declaration used the `vararg` modifier.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_vararg: bool,
    /// When the param type is a lambda-with-receiver (`Foo.() -> R`),
    /// this is the simple receiver class name (`Foo`). Used by the
    /// cross-file call-site dispatcher to set
    /// `MirModule::lambda_receiver_type` before lowering the lambda
    /// arg, so the lambda body's bare-name method calls
    /// (`add(1)` / `child(...)`) resolve as `this.add(1)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_class: Option<String>,
}

impl ExternalParam {
    pub fn new(name: impl Into<String>, ty: Ty) -> Self {
        Self {
            name: name.into(),
            ty,
            has_default: false,
            is_vararg: false,
            receiver_class: None,
        }
    }
}

/// A cross-file method signature with full per-parameter metadata.
/// Replaces the historical `(String, Vec<Ty>, Ty)` tuple — every
/// surface that used those needs at minimum the return type and the
/// param types, but inline/default-mask/vararg/receiver_ty drive
/// real call-site dispatch decisions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalMethod {
    pub name: String,
    pub params: Vec<ExternalParam>,
    pub return_ty: Ty,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_suspend: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_inline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_abstract: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_open: bool,
    /// Extension-fn receiver type when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_ty: Option<Ty>,
    /// Annotations on this method (simple-name strings — e.g.
    /// `"JvmStatic"`, `"Composable"`, `"JvmField"`). Surfaced so
    /// cross-file call sites can branch on annotations the way
    /// in-file lowering already does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
}

impl ExternalMethod {
    /// Returns just the parameter types — the historical shape consumers
    /// expect from `.methods.iter().map(|(_, p, _)| p)`.
    pub fn param_tys(&self) -> Vec<Ty> {
        self.params.iter().map(|p| p.ty.clone()).collect()
    }
}

/// A cross-file constructor signature. Primary and secondary
/// constructors share this shape — the primary one lives in
/// `ExternalClassDecl.primary_ctor`, the rest in `secondary_ctors`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalConstructor {
    pub params: Vec<ExternalParam>,
}

/// Metadata for a top-level function from another file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalFunDecl {
    /// JVM internal name of the wrapper class, e.g. "com/example/GreeterKt".
    pub owner_class: String,
    /// JVM method descriptor, e.g. "(Ljava/lang/String;)I".
    pub descriptor: String,
    /// Return type for the typechecker.
    pub return_ty: Ty,
    /// Parameter types for the typechecker.
    pub param_tys: Vec<Ty>,
    /// Number of declared parameters (excluding receiver for extensions).
    pub param_count: usize,
    /// True if declared with `suspend`.
    pub is_suspend: bool,
    /// True if declared with `inline` — surfaced so cross-file inline
    /// callers can route through the body-splicing path the in-file
    /// inliner uses.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_inline: bool,
    /// True if this is an extension function.
    pub is_extension: bool,
    /// Extension-fn receiver type when present (`fun String.exclaim()` →
    /// `Some(Ty::String)`). Used to disambiguate overloads where the
    /// same name is declared on multiple receivers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_ty: Option<Ty>,
    /// Per-param `has_default` bits. Same length as `param_tys`. Drives
    /// `$default(... I)` dispatch so callers that omit defaultable
    /// arguments pick the right descriptor.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub has_default: Vec<bool>,
    /// Per-param `vararg` bits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub is_vararg: Vec<bool>,
    /// Annotation simple-names on this function (`"JvmStatic"`,
    /// `"Composable"`, `"Deprecated"`, etc).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
}

/// Metadata for a top-level val from another file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalValDecl {
    pub owner_class: String,
    pub ty: Ty,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
}

/// Metadata for a class/object/enum/interface from another file.
///
/// One canonical shape across every `ExternalClassKind` — each field
/// is populated when applicable to that kind (and left empty / `false`
/// when not). Downstream consumers (typeck cross-file stub builder,
/// mir-lower stub MirClass builder) treat all kinds uniformly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalClassDecl {
    /// Fully-qualified JVM internal name, e.g. "com/example/Greeter".
    pub jvm_name: String,
    pub kind: ExternalClassKind,
    /// Constructor parameter fields (val/var in primary constructor).
    pub fields: Vec<(String, Ty)>,
    /// All primary-constructor parameters in declaration order — name +
    /// type for every param, val/var or not. Cross-file call sites use
    /// this to (a) build a complete `<init>` descriptor when the class
    /// has plain (non-field) constructor params, and (b) reorder named
    /// args to positional order. `fields` is a subset (val/var only),
    /// kept for callers that just need the storage layout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ctor_params: Vec<ExternalParam>,
    /// Method signatures. Now carries enough metadata that consumers
    /// can branch on `is_inline`/`is_suspend`/etc. directly rather than
    /// inferring from names. Includes synthesized property getters
    /// (`getX` / `setX`).
    pub methods: Vec<ExternalMethod>,
    /// Secondary constructors. Each one drives a separate `<init>`
    /// descriptor at the call site. Empty for kinds that can't have
    /// them (Object, Enum, Interface).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_ctors: Vec<ExternalConstructor>,
    /// Companion-object method signatures, used so call sites that go
    /// through `OuterClass.method(...)` can resolve to the companion's
    /// virtual method via `OuterClass$Companion`. Empty when the class
    /// has no `companion object`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companion_methods: Vec<ExternalMethod>,
    /// True when the source declaration carried a `companion object`
    /// block. Distinguishes "class with empty companion" from "class
    /// with no companion at all" — `companion_methods.is_empty()`
    /// alone collapsed those two cases together.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_companion: bool,
    /// Superclass name (simple, not FQ). For enums this is always
    /// `Some("kotlin/Enum".into())` even though source doesn't
    /// spell it.
    pub super_class: Option<String>,
    /// Interface names this class/object implements (simple, not FQ).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,
    /// Whether the class is open.
    pub is_open: bool,
    /// Whether the class is abstract.
    pub is_abstract: bool,
    /// Whether the class was declared `inner` (holds an implicit
    /// outer-instance reference at every instance, so `<init>` takes
    /// an extra leading `outer` param at the JVM level).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_inner: bool,
    /// Source-declared enum entries in declaration order — present
    /// only for `Enum` kind. Used cross-file by `when` exhaustiveness
    /// checking and by `EnumName.ENTRY` static-field dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_entries: Vec<String>,
    /// Annotations on the class declaration itself (`@Composable`,
    /// `@Deprecated`, etc).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
    /// True when the class has type parameters (`class Box<T>(...)`).
    /// Needed cross-file because callers must emit a `Signature`
    /// attribute when the param or return types reference a generic
    /// external class.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_type_params: bool,
    /// True when the source declaration carries one or more `init { }`
    /// blocks. The blocks themselves are not surfaced cross-file (the
    /// constructor body lives in the producing file), but downstream
    /// purity / side-effect analyses need to know the `<init>` is not
    /// a pure field assignment so they don't accidentally treat the
    /// class as `data`-class-equivalent for short-circuit equality or
    /// similar reasoning.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_init_blocks: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExternalClassKind {
    Class,
    DataClass,
    Object,
    Enum,
    Interface,
    SealedClass,
    SealedInterface,
}

// ── Gather pass ─────────────────────────────────────────────────────

/// Map an AST `TypeRef` name to a JVM type descriptor character/string.
/// When `param_position` is true, `Unit` maps to `Lkotlin/Unit;` (not `V`,
/// which is only valid as a return-type descriptor).
fn type_ref_to_descriptor_with_aliases(
    tr: &TypeRef,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> String {
    type_ref_to_descriptor_inner(tr, interner, false, imports, aliases)
}

fn type_ref_to_param_descriptor_with_aliases(
    tr: &TypeRef,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> String {
    type_ref_to_descriptor_inner(tr, interner, true, imports, aliases)
}

fn type_ref_to_descriptor_inner(
    tr: &TypeRef,
    interner: &Interner,
    param_position: bool,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> String {
    // Function types: (P1, P2, ...) -> R  →  Lkotlin/jvm/functions/FunctionN;
    // `@Composable` bumps arity by 2 (Composer + Int $changed); `suspend`
    // bumps arity by 1 (Continuation). Without these bumps, cross-file
    // callers see the wrong Function shape in the descriptor and
    // checkcast the lambda to the wrong interface at runtime.
    if tr.func_params.is_some() {
        let base = tr.func_params.as_ref().map_or(0, |v| v.len());
        let arity = base + if tr.is_composable { 2 } else { 0 } + if tr.is_suspend { 1 } else { 0 };
        return format!("Lkotlin/jvm/functions/Function{arity};");
    }
    let name = interner.resolve(tr.name);
    // Typealias substitution — `typealias Predicate = (Int) -> Boolean`
    // expands the field/param's descriptor to FunctionN, not the
    // return-type's name. Without this, cross-file callers built
    // `Object` descriptors for typealias-typed slots and the JVM
    // failed to find the constructor / method.
    if !tr.nullable {
        if let Some(target_tr) = aliases.get(name) {
            return type_ref_to_descriptor_inner(
                target_tr,
                interner,
                param_position,
                imports,
                aliases,
            );
        }
    }
    if tr.nullable {
        return "Ljava/lang/Object;".to_string();
    }
    match name {
        "Int" => "I".to_string(),
        "Long" => "J".to_string(),
        "Double" => "D".to_string(),
        "Float" => "F".to_string(),
        "Boolean" => "Z".to_string(),
        "Byte" => "B".to_string(),
        "Short" => "S".to_string(),
        "Char" => "C".to_string(),
        "Unit" if param_position => "Lkotlin/Unit;".to_string(),
        "Unit" => "V".to_string(),
        "String" => "Ljava/lang/String;".to_string(),
        "IntArray" => "[I".to_string(),
        "LongArray" => "[J".to_string(),
        "DoubleArray" => "[D".to_string(),
        "BooleanArray" => "[Z".to_string(),
        "ByteArray" => "[B".to_string(),
        _ => {
            // Resolve user-class types via the file's import map. Without
            // this, every non-builtin class falls back to
            // `Ljava/lang/Object;` and cross-file callers emit descriptors
            // that don't match the producer's `L<package>/Class;` —
            // resulting in `NoSuchMethodError` at runtime even though
            // both sides "have" the method. (E.g. JetChat's
            // `JetchatDrawer(DrawerState, ...)` produced
            // `Object` callers vs `DrawerState` declaration.)
            if let Some(fq) = imports.get(name) {
                format!("L{fq};")
            } else if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
                // Kotlin builtin names (`MutableMap`, `MutableList`, …)
                // erase to their `java/util/*` JVM equivalents at the
                // descriptor boundary — mirrors the same lookup in
                // `type_ref_to_ty_with_aliases`. Without this, a
                // `fun foo(): MutableMap<K, V>` cross-file callee gets
                // descriptor `()Ljava/lang/Object;` on the caller side
                // but `()Ljava/util/Map;` on the callee — runtime
                // NoSuchMethodError.
                format!("L{jvm};")
            } else {
                "Ljava/lang/Object;".to_string()
            }
        }
    }
}

/// Best-effort return-type inference for expression-body methods
/// declared without an explicit `return_ty`. Used by gather_declarations
/// so cross-file callers see the right return type (`Boolean` for
/// `fun isMe() = userId == meProfile.userId`, not `Unit`). The full
/// typeck pass already handles inference on the producer side, but at
/// gather_declarations time only the AST is available, and falling back
/// to `Unit` breaks call-site descriptors — e.g. JetChat's
/// `userData.isMe()` was lowered as `()V`, leaving nothing on the stack
/// for the next composable arg and triggering d8 stack-map rejects.
fn infer_body_return_ty(body: &skotch_syntax::Block, interner: &Interner) -> Ty {
    use skotch_syntax::{BinOp, Expr, Stmt};
    // Block body with no `return value` statement returns Unit, even
    // when the last statement is an expression — Kotlin discards the
    // value of a trailing Stmt::Expr. Inferring as `Ty::Any` (the old
    // behavior) made instance method calls like `inventory.add(x, y)`
    // emit descriptors with `Ljava/lang/Object;` returns where the real
    // method returns void — JVM threw NoSuchMethodError at the call
    // site. Only when an explicit `return value` is present do we infer
    // the returned expression's type.
    let returned_expr = body.stmts.iter().rev().find_map(|s| match s {
        Stmt::Return { value: Some(e), .. } => Some(e),
        _ => None,
    });
    let expr = match returned_expr {
        Some(e) => e,
        None => return Ty::Unit,
    };
    match expr {
        Expr::Binary { op, .. } => match op {
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::LtEq
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or => Ty::Bool,
            _ => Ty::Any,
        },
        Expr::BoolLit(_, _) => Ty::Bool,
        Expr::IntLit(_, _) => Ty::Int,
        Expr::LongLit(_, _) => Ty::Long,
        Expr::DoubleLit(_, _) => Ty::Double,
        Expr::FloatLit(_, _) => Ty::Float,
        Expr::StringLit(_, _) | Expr::StringTemplate(_, _) => Ty::String,
        Expr::CharLit(_, _) => Ty::Char,
        _ => {
            let _ = interner;
            Ty::Any
        }
    }
}

/// Map an AST `TypeRef` to a `Ty` for the typechecker.
/// Best-effort type inference for a `val foo = <init>` without an explicit
/// type annotation. We only look one level deep — the goal is to recover the
/// JVM descriptor of the getter accessor (which cross-file `name` lookups
/// turn into `getFoo()<descriptor>`), not full type inference.
///
/// Recognized patterns:
/// * `val foo = Bar(...)` where `Bar` resolves to a class in `imports` →
///   `Ty::Class(<fq>)`.
/// * `val foo = "literal"` / number / bool → matching primitive `Ty`.
///
/// Everything else falls back to `Ty::Any`, matching the previous behavior.
fn infer_val_type_from_init(
    init: &skotch_syntax::ast::Expr,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
) -> Ty {
    use skotch_syntax::ast::Expr;
    match init {
        Expr::Call { callee, args, .. } => match callee.as_ref() {
            Expr::Ident(sym, _) => {
                let name = interner.resolve(*sym);
                // Kotlin stdlib collection builders + Compose state
                // holders — centralized in
                // `skotch_types::intrinsics::fallback_collection_builder_class`.
                // We pick the first arg's inferred type as the
                // element; heterogeneous `listOf(1, "two")` would
                // infer `Any` either way, and the JVM-erased
                // descriptor is still `List`.
                if skotch_types::intrinsics::fallback_collection_builder_class(name).is_some() {
                    // `mapOf("k" to 1, ...)` args are `Pair<K,V>`, but we
                    // don't yet trace through `to`; the first arg's inferred
                    // type signals there IS a parameterization and downstream
                    // code falls back to erasure if it can't use it.
                    let elem_ty = args
                        .first()
                        .map(|a| infer_val_type_from_init(&a.expr, interner, imports))
                        .unwrap_or(Ty::Any);
                    return skotch_types::intrinsics::collection_builder_result_ty(name, elem_ty)
                        .unwrap_or(Ty::Any);
                }
                // Only treat the call as a constructor when the callee name
                // starts uppercase (Kotlin convention). Lowercase Ident calls
                // are top-level functions whose return type we can't recover
                // here, and pretending the function name IS its return type
                // produces fields like `ThemesKt.JetchatDarkColorScheme:
                // Landroidx/compose/material3/darkColorScheme;` (the
                // function path stuffed into a class-type slot).
                let starts_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                if starts_upper {
                    if let Some(fq) = imports.get(name) {
                        Ty::Class(fq.clone())
                    } else {
                        Ty::Any
                    }
                } else {
                    Ty::Any
                }
            }
            _ => Ty::Any,
        },
        Expr::StringLit(_, _) | Expr::StringTemplate(_, _) => {
            Ty::Class("java/lang/String".to_string())
        }
        Expr::IntLit(_, _) => Ty::Int,
        Expr::LongLit(_, _) => Ty::Long,
        Expr::FloatLit(_, _) => Ty::Float,
        Expr::DoubleLit(_, _) => Ty::Double,
        Expr::BoolLit(_, _) => Ty::Bool,
        Expr::CharLit(_, _) => Ty::Char,
        _ => Ty::Any,
    }
}

fn type_ref_to_ty_with_aliases(
    tr: &TypeRef,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> Ty {
    // Function type: `(P1, ...) -> R` → `Ty::Function`. Must come
    // before the typealias substitution and name lookups so
    // explicit function-type TypeRefs are recognised here too.
    if tr.func_params.is_some() {
        let params: Vec<Ty> = tr
            .func_params
            .as_ref()
            .map(|fps| {
                fps.iter()
                    .map(|p| type_ref_to_ty_with_aliases(p, interner, imports, aliases))
                    .collect()
            })
            .unwrap_or_default();
        let ret_tr = TypeRef {
            name: tr.name,
            nullable: false,
            func_params: None,
            type_args: Vec::new(),
            is_suspend: false,
            is_composable: false,
            has_receiver: false,
            span: tr.span,
        };
        let ret = type_ref_to_ty_with_aliases(&ret_tr, interner, imports, aliases);
        let base = Ty::Function {
            params,
            ret: Box::new(ret),
            is_suspend: tr.is_suspend,
            is_composable: tr.is_composable,
        };
        return if tr.nullable {
            Ty::Nullable(Box::new(base))
        } else {
            base
        };
    }
    let name = interner.resolve(tr.name);
    // Typealias substitution — `typealias Predicate = (Int) -> Boolean`
    // needs the resolved Ty (Function) rather than the alias's name.
    if !tr.nullable {
        if let Some(target_tr) = aliases.get(name) {
            return type_ref_to_ty_with_aliases(target_tr, interner, imports, aliases);
        }
    }
    let base = skotch_types::ty_from_name(name).unwrap_or_else(|| {
        // Kotlin built-in class names that map to a JVM class via
        // JavaToKotlinClassMap (`List` → `java/util/List`, `CharSequence`
        // → `java/lang/CharSequence`, the exception hierarchy, …). Without
        // this a cross-file constructor/method param declared
        // `List<Message>` erased to `Any`, so the recorded
        // `ExternalClass.ctor_params` type was `Object` and the cross-file
        // `<init>` descriptor came out `(…,Ljava/lang/Object;)V` instead
        // of `(…,Ljava/util/List;)V` — a runtime NoSuchMethodError (this
        // is JetChat's `ConversationUiState(…, initialMessages: List<…>)`).
        if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
            Ty::Class(jvm.to_string())
        } else if let Some(fq) = imports.get(name) {
            // Fall back to the file's import_map so a cross-file call site
            // sees the right param/return type (e.g. `LayoutInflater` →
            // `Ty::Class("android/view/LayoutInflater")`). Without this,
            // `gather_declarations` erases everything non-builtin to `Any`
            // and downstream methodref descriptors become
            // `(Object,Object,…)Object`, breaking JVM resolution.
            Ty::Class(fq.clone())
        } else {
            Ty::Any
        }
    });
    if tr.nullable {
        Ty::Nullable(Box::new(base))
    } else {
        base
    }
}

fn build_descriptor_with_aliases(
    params: &[Param],
    return_ty: Option<&TypeRef>,
    receiver_ty: Option<&TypeRef>,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> String {
    let mut desc = String::from("(");
    if let Some(recv) = receiver_ty {
        desc.push_str(&type_ref_to_param_descriptor_with_aliases(
            recv, interner, imports, aliases,
        ));
    }
    for p in params {
        desc.push_str(&type_ref_to_param_descriptor_with_aliases(
            &p.ty, interner, imports, aliases,
        ));
    }
    desc.push(')');
    if let Some(ret) = return_ty {
        desc.push_str(&type_ref_to_descriptor_with_aliases(
            ret, interner, imports, aliases,
        ));
    } else {
        desc.push('V');
    }
    desc
}

/// Resolve a Kotlin annotation to its simple name, applying the
/// well-known `kotlin/jvm/JvmStatic` → `JvmStatic` collapse so the
/// downstream `annotations: Vec<String>` lists are normalized.
fn annotation_name(a: &Annotation, interner: &Interner) -> String {
    let raw = interner.resolve(a.name);
    // The two equivalent spellings — strip the kotlin/jvm/ FQ prefix so
    // cross-file annotation lookups don't depend on which form the user
    // wrote.
    raw.rsplit('/').next().unwrap_or(raw).to_string()
}

fn annotations_to_strings(annots: &[Annotation], interner: &Interner) -> Vec<String> {
    annots
        .iter()
        .map(|a| annotation_name(a, interner))
        .collect()
}

/// Build an `ExternalParam` from an AST `Param`.
fn ext_param_from_param(
    p: &Param,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> ExternalParam {
    let receiver_class = if p.ty.has_receiver {
        p.ty.func_params
            .as_ref()
            .and_then(|fps| fps.first())
            .map(|recv| interner.resolve(recv.name).to_string())
    } else {
        None
    };
    ExternalParam {
        name: interner.resolve(p.name).to_string(),
        ty: type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases),
        has_default: p.default.is_some(),
        is_vararg: p.is_vararg,
        receiver_class,
    }
}

/// Build an `ExternalParam` from an AST `ConstructorParam`.
fn ext_param_from_ctor_param(
    p: &ConstructorParam,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> ExternalParam {
    ExternalParam {
        name: interner.resolve(p.name).to_string(),
        ty: type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases),
        // `ConstructorParam` doesn't carry a default-value field today;
        // when it does, populate here.
        has_default: false,
        is_vararg: false,
        receiver_class: None,
    }
}

/// Build an `ExternalMethod` from an AST `FunDecl`.
fn ext_method_from_fun(
    f: &FunDecl,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> ExternalMethod {
    let params: Vec<ExternalParam> = f
        .params
        .iter()
        .map(|p| ext_param_from_param(p, interner, imports, aliases))
        .collect();
    let return_ty = f
        .return_ty
        .as_ref()
        .map(|tr| type_ref_to_ty_with_aliases(tr, interner, imports, aliases))
        .unwrap_or_else(|| infer_body_return_ty(&f.body, interner));
    let receiver_ty = f
        .receiver_ty
        .as_ref()
        .map(|tr| type_ref_to_ty_with_aliases(tr, interner, imports, aliases));
    ExternalMethod {
        name: interner.resolve(f.name).to_string(),
        params,
        return_ty,
        is_suspend: f.is_suspend,
        is_inline: f.is_inline,
        is_abstract: f.is_abstract,
        is_open: f.is_open,
        receiver_ty,
        annotations: annotations_to_strings(&f.annotations, interner),
    }
}

/// Build the synthesized `getX()` accessor method for a body property
/// declaration. Returns `None` for `@JvmField`-annotated properties
/// (those are exposed as bare fields, no synthesized getter).
fn property_getter_method(
    p: &skotch_syntax::PropertyDecl,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> Option<ExternalMethod> {
    let is_jvm_field = p.annotations.iter().any(|a| {
        let n = interner.resolve(a.name);
        n == "JvmField" || n == "kotlin/jvm/JvmField"
    });
    if is_jvm_field {
        return None;
    }
    let pname = interner.resolve(p.name).to_string();
    let mut chars = pname.chars();
    let getter_name = match chars.next() {
        Some(c) => format!(
            "get{}{}",
            c.to_uppercase().collect::<String>(),
            chars.as_str()
        ),
        None => return None,
    };
    let ret =
        p.ty.as_ref()
            .map(|tr| type_ref_to_ty_with_aliases(tr, interner, imports, aliases))
            .unwrap_or(Ty::Any);
    Some(ExternalMethod {
        name: getter_name,
        params: Vec::new(),
        return_ty: ret,
        is_suspend: false,
        is_inline: false,
        is_abstract: false,
        is_open: false,
        receiver_ty: None,
        annotations: annotations_to_strings(&p.annotations, interner),
    })
}

/// Build the constructor parameter shape (full list + the val/var
/// subset that becomes accessible fields). `fields` honors visibility
/// — `private val x` is reachable in-file but not cross-file.
fn build_class_ctor_shape(
    params: &[ConstructorParam],
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
) -> (Vec<(String, Ty)>, Vec<ExternalParam>) {
    let fields: Vec<(String, Ty)> = params
        .iter()
        .filter(|p| {
            // Only val/var ctor params become accessible fields. Private
            // ones are unreachable cross-file — they remain class-internal.
            (p.is_val || p.is_var) && !ctor_param_is_private(p, interner)
        })
        .map(|p| {
            (
                interner.resolve(p.name).to_string(),
                type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases),
            )
        })
        .collect();
    let ctor_params: Vec<ExternalParam> = params
        .iter()
        .map(|p| ext_param_from_ctor_param(p, interner, imports, aliases))
        .collect();
    (fields, ctor_params)
}

/// Detect a `@private` or visibility-marker annotation on a
/// `ConstructorParam`. Kotlin only tags ctor-param visibility via the
/// `is_val`/`is_var` prefix modifier or per-param `@private` / `private`
/// keyword. The current parser collects per-param annotations but
/// doesn't surface a separate `visibility` field on `ConstructorParam`,
/// so we look for the well-known `@JvmStatic` / `private` annotations.
fn ctor_param_is_private(p: &ConstructorParam, interner: &Interner) -> bool {
    p.annotations
        .iter()
        .any(|a| matches!(interner.resolve(a.name), "private" | "Private"))
}

/// Build the per-class supertype simple-name list, threaded through
/// the same alias/import handling everywhere else uses.
fn build_supertypes(
    parent_class: Option<&skotch_syntax::SuperClassRef>,
    interfaces: &[Symbol],
    interner: &Interner,
) -> (Option<String>, Vec<String>) {
    let super_class = parent_class.map(|sc| interner.resolve(sc.name).to_string());
    let iface_names = interfaces
        .iter()
        .map(|s| interner.resolve(*s).to_string())
        .collect();
    (super_class, iface_names)
}

/// Recursively gather a class declaration (and any nested classes
/// inside it) into the package table. Nested classes are registered
/// with `Outer$Inner` JVM names so cross-file callers can construct
/// them as `Outer.Inner(...)`. The recursion is breadth-first per
/// nesting level: outer class first, then each direct nested, then
/// each nested's own nested, etc.
#[allow(clippy::too_many_arguments)]
fn gather_class_recursive(
    c: &ClassDecl,
    fq_outer: &str,
    table: &mut PackageSymbolTable,
    interner: &Interner,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, TypeRef>,
    diags: &mut Diagnostics,
) {
    if c.visibility == Visibility::Private {
        return;
    }
    let simple_name = interner.resolve(c.name).to_string();
    let kind = if c.is_sealed {
        ExternalClassKind::SealedClass
    } else if c.is_data {
        ExternalClassKind::DataClass
    } else {
        ExternalClassKind::Class
    };
    let jvm_name_pre = format!("{fq_outer}{simple_name}");
    // Per-class imports overlay: includes nested classes so a body
    // method's return or param type that names a sibling nested class
    // (`fun makeNested(): Nested = ...`) resolves to `Outer$Nested`
    // rather than collapsing to Any. Without this, the cross-file
    // method descriptor would be `(I)Ljava/lang/Object;` while the
    // real bytecode is `(I)LOuter$Nested;` and the runtime fails with
    // NoSuchMethodError.
    let mut class_imports = imports.clone();
    for n in &c.nested_classes {
        let nested_simple = interner.resolve(n.name).to_string();
        let nested_jvm = format!("{jvm_name_pre}${nested_simple}");
        class_imports.entry(nested_simple).or_insert(nested_jvm);
    }
    let imports = &class_imports;
    let (fields, ctor_params) =
        build_class_ctor_shape(&c.constructor_params, interner, imports, aliases);
    let property_getters: Vec<ExternalMethod> = c
        .properties
        .iter()
        .filter_map(|p| property_getter_method(p, interner, imports, aliases))
        .collect();
    let mut methods: Vec<ExternalMethod> = c
        .methods
        .iter()
        .map(|m| ext_method_from_fun(m, interner, imports, aliases))
        .collect();
    methods.extend(property_getters);
    let companion_property_getters: Vec<ExternalMethod> = c
        .companion_properties
        .iter()
        .filter_map(|p| property_getter_method(p, interner, imports, aliases))
        .collect();
    let mut companion_methods: Vec<ExternalMethod> = c
        .companion_methods
        .iter()
        .map(|m| ext_method_from_fun(m, interner, imports, aliases))
        .collect();
    companion_methods.extend(companion_property_getters);
    let has_companion = !c.companion_methods.is_empty() || !c.companion_properties.is_empty();
    let secondary_ctors: Vec<ExternalConstructor> = c
        .secondary_constructors
        .iter()
        .map(|sc| ExternalConstructor {
            params: sc
                .params
                .iter()
                .map(|p| ext_param_from_param(p, interner, imports, aliases))
                .collect(),
        })
        .collect();
    let (super_class, iface_names) =
        build_supertypes(c.parent_class.as_ref(), &c.interfaces, interner);
    let jvm_name = jvm_name_pre;
    let ext = ExternalClassDecl {
        jvm_name: jvm_name.clone(),
        kind,
        fields,
        ctor_params,
        methods,
        secondary_ctors,
        companion_methods,
        has_companion,
        super_class,
        interfaces: iface_names,
        is_open: c.is_open,
        is_abstract: c.is_abstract,
        is_inner: c.is_inner,
        enum_entries: Vec::new(),
        annotations: annotations_to_strings(&c.annotations, interner),
        has_type_params: !c.type_params.is_empty(),
        has_init_blocks: !c.init_blocks.is_empty(),
    };
    register_class_in_table(table, &simple_name, ext, diags);
    // Recurse into nested classes — each becomes `Outer$Inner` at the
    // JVM level. Their inner classes nest further, e.g.
    // `Outer$Inner$Deeper`.
    let nested_outer = format!("{jvm_name}$");
    for n in &c.nested_classes {
        gather_class_recursive(n, &nested_outer, table, interner, imports, aliases, diags);
    }
}

/// Insert an `ExternalClassDecl` into the table. Keyed by simple name
/// (the lookup shape most consumers expect — `Foo` from an AST Ident
/// resolves into the `Foo` ExternalClassDecl). Also indexes by FQ name
/// in `classes_by_fq` so callers with a JVM-internal name in hand can
/// disambiguate between same-simple-named classes in different
/// sub-packages.
fn register_class_in_table(
    table: &mut PackageSymbolTable,
    simple_name: &str,
    ext: ExternalClassDecl,
    _diags: &mut Diagnostics,
) {
    let fq = ext.jvm_name.clone();
    // Both indices point to the same data; keep them in sync.
    table
        .simple_name_to_fq
        .insert(simple_name.to_string(), fq.clone());
    table.classes_by_fq.insert(fq, ext.clone());
    table.classes.insert(simple_name.to_string(), ext);
}

/// Gather all top-level declarations from multiple parsed files into a
/// shared [`PackageSymbolTable`]. This is Phase 1 of multi-file compilation.
///
/// Each entry in `files` is `(file_id, parsed_ast, wrapper_class_name)`.
pub fn gather_declarations(
    files: &[(FileId, &KtFile, &str)],
    interner: &Interner,
) -> PackageSymbolTable {
    let mut diags = Diagnostics::default();
    let mut table = PackageSymbolTable::default();
    let diags = &mut diags;

    for (_file_id, ast, wrapper_class) in files {
        // Compute package prefix for FQ JVM names.
        let pkg_prefix = if let Some(ref pkg) = ast.package {
            let segments: Vec<&str> = pkg.path.iter().map(|s| interner.resolve(*s)).collect();
            if segments.is_empty() {
                String::new()
            } else {
                format!("{}/", segments.join("/"))
            }
        } else {
            String::new()
        };
        let fq_wrapper = format!("{pkg_prefix}{wrapper_class}");

        // Per-file simple-name → FQ JVM-name map for class references in
        // declarations of this file. Cross-file callers see descriptors
        // emitted here; if `DrawerState` isn't resolved we fall back to
        // `Object` and the descriptor doesn't match the producer.
        let mut file_imports: FxHashMap<String, String> = FxHashMap::default();
        for imp in &ast.imports {
            if imp.is_wildcard {
                continue;
            }
            let segments: Vec<&str> = imp.path.iter().map(|s| interner.resolve(*s)).collect();
            if segments.is_empty() {
                continue;
            }
            let fq = segments.join("/");
            // The alias (if any) wins; otherwise the last segment is the
            // simple name. Skip empty names.
            let simple = if let Some(a) = imp.alias {
                interner.resolve(a).to_string()
            } else {
                segments.last().unwrap_or(&"").to_string()
            };
            if !simple.is_empty() {
                file_imports.insert(simple, fq);
            }
        }
        // Add same-package classes declared in any of the files passed in,
        // so a declaration in this file can reference a class declared
        // beside it without an explicit import.
        let mut file_aliases: FxHashMap<String, TypeRef> = FxHashMap::default();
        for (_, other_ast, _other_wrapper) in files {
            if other_ast.package.as_ref().map(|p| &p.path) != ast.package.as_ref().map(|p| &p.path)
            {
                continue;
            }
            for d in &other_ast.decls {
                let simple_opt = match d {
                    Decl::Class(c) => Some(interner.resolve(c.name).to_string()),
                    Decl::Enum(e) => Some(interner.resolve(e.name).to_string()),
                    Decl::Interface(i) => Some(interner.resolve(i.name).to_string()),
                    Decl::Object(o) => Some(interner.resolve(o.name).to_string()),
                    _ => None,
                };
                if let Some(simple) = simple_opt {
                    file_imports
                        .entry(simple.clone())
                        .or_insert_with(|| format!("{pkg_prefix}{simple}"));
                }
                // Also surface same-package typealias declarations so
                // descriptor building can substitute the alias's
                // target TypeRef (`typealias Predicate = (Int) -> Boolean`
                // — used in CountingMatcher's primary ctor — must
                // emit `LFunction1;` not `Ljava/lang/Object;`).
                if let Decl::TypeAlias(ta) = d {
                    let alias_name = interner.resolve(ta.name).to_string();
                    file_aliases
                        .entry(alias_name)
                        .or_insert_with(|| ta.target.clone());
                }
            }
        }
        let imports = &file_imports;
        let aliases = &file_aliases;

        for decl in &ast.decls {
            match decl {
                Decl::Fun(f) => {
                    // Private functions are file-scoped — not visible cross-file.
                    if f.visibility == Visibility::Private {
                        continue;
                    }
                    let name = interner.resolve(f.name).to_string();
                    let mut descriptor = build_descriptor_with_aliases(
                        &f.params,
                        f.return_ty.as_ref(),
                        f.receiver_ty.as_ref(),
                        interner,
                        imports,
                        aliases,
                    );
                    // @Composable functions get $composer/$changed params added
                    // by the compose transform. Include them in the descriptor
                    // so cross-file call sites use the correct param count.
                    let is_composable = f
                        .annotations
                        .iter()
                        .any(|a| interner.resolve(a.name) == "Composable");
                    if is_composable {
                        if let Some(close_paren) = descriptor.rfind(')') {
                            descriptor
                                .insert_str(close_paren, "Landroidx/compose/runtime/Composer;I");
                        }
                    }
                    let return_ty = f
                        .return_ty
                        .as_ref()
                        .map(|tr| type_ref_to_ty_with_aliases(tr, interner, imports, aliases))
                        .unwrap_or(Ty::Unit);
                    let param_tys: Vec<Ty> = f
                        .params
                        .iter()
                        .map(|p| type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases))
                        .collect();
                    let param_count = if is_composable {
                        f.params.len() + 2
                    } else {
                        f.params.len()
                    };
                    let has_default: Vec<bool> =
                        f.params.iter().map(|p| p.default.is_some()).collect();
                    let is_vararg: Vec<bool> = f.params.iter().map(|p| p.is_vararg).collect();
                    let receiver_ty = f
                        .receiver_ty
                        .as_ref()
                        .map(|tr| type_ref_to_ty_with_aliases(tr, interner, imports, aliases));
                    let ext = ExternalFunDecl {
                        owner_class: fq_wrapper.clone(),
                        descriptor,
                        return_ty,
                        param_count,
                        param_tys,
                        is_suspend: f.is_suspend,
                        is_inline: f.is_inline,
                        is_extension: f.receiver_ty.is_some(),
                        receiver_ty,
                        has_default,
                        is_vararg,
                        annotations: annotations_to_strings(&f.annotations, interner),
                    };
                    table.functions.entry(name).or_default().push(ext);
                }
                Decl::Val(v) => {
                    if v.visibility == Visibility::Private {
                        continue;
                    }
                    let name = interner.resolve(v.name).to_string();
                    let ty = match v.ty.as_ref() {
                        Some(tr) => type_ref_to_ty_with_aliases(tr, interner, imports, aliases),
                        None => infer_val_type_from_init(&v.init, interner, imports),
                    };
                    table.vals.insert(
                        name,
                        ExternalValDecl {
                            owner_class: fq_wrapper.clone(),
                            ty,
                            annotations: annotations_to_strings(&v.annotations, interner),
                        },
                    );
                }
                Decl::Class(c) => {
                    gather_class_recursive(
                        c,
                        &pkg_prefix,
                        &mut table,
                        interner,
                        imports,
                        aliases,
                        diags,
                    );
                }
                Decl::Object(o) => {
                    let simple_name = interner.resolve(o.name).to_string();
                    let methods: Vec<ExternalMethod> = o
                        .methods
                        .iter()
                        .map(|m| ext_method_from_fun(m, interner, imports, aliases))
                        .collect();
                    let (super_class, iface_names) =
                        build_supertypes(o.parent_class.as_ref(), &o.interfaces, interner);
                    let jvm_name = format!("{pkg_prefix}{simple_name}");
                    let ext = ExternalClassDecl {
                        jvm_name,
                        kind: ExternalClassKind::Object,
                        fields: Vec::new(),
                        ctor_params: Vec::new(),
                        methods,
                        secondary_ctors: Vec::new(),
                        companion_methods: Vec::new(),
                        has_companion: false,
                        super_class,
                        interfaces: iface_names,
                        is_open: false,
                        is_abstract: false,
                        is_inner: false,
                        enum_entries: Vec::new(),
                        annotations: Vec::new(),
                        has_type_params: false,
                        has_init_blocks: false,
                    };
                    register_class_in_table(&mut table, &simple_name, ext, diags);
                }
                Decl::Enum(e) => {
                    let simple_name = interner.resolve(e.name).to_string();
                    // Enum body methods become real virtual methods on
                    // the enum class. Per-entry anonymous-class overrides
                    // live on `EnumName$EntryName` subclasses but the
                    // overall method set is dispatched off the enum
                    // class itself, so surface the body methods here.
                    let methods: Vec<ExternalMethod> = e
                        .methods
                        .iter()
                        .map(|m| ext_method_from_fun(m, interner, imports, aliases))
                        .collect();
                    let (_, ctor_params) =
                        build_class_ctor_shape(&e.constructor_params, interner, imports, aliases);
                    // Enum-class fields are derived from val/var ctor
                    // params (just like a regular class).
                    let fields: Vec<(String, Ty)> = e
                        .constructor_params
                        .iter()
                        .filter(|p| (p.is_val || p.is_var) && !ctor_param_is_private(p, interner))
                        .map(|p| {
                            (
                                interner.resolve(p.name).to_string(),
                                type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases),
                            )
                        })
                        .collect();
                    let enum_entries: Vec<String> = e
                        .entries
                        .iter()
                        .map(|en| interner.resolve(en.name).to_string())
                        .collect();
                    let (_super_class, iface_names) =
                        build_supertypes(None, &e.interfaces, interner);
                    // Enums always extend `java/lang/Enum` at the JVM
                    // level (kotlin/Enum is the source-level name; the
                    // Kotlin-to-Java class map translates it to
                    // java/lang/Enum). Record it even though source
                    // doesn't spell it so cross-file callers see the
                    // correct supertype.
                    let super_class = Some("java/lang/Enum".to_string());
                    let jvm_name = format!("{pkg_prefix}{simple_name}");
                    let ext = ExternalClassDecl {
                        jvm_name,
                        kind: ExternalClassKind::Enum,
                        fields,
                        ctor_params,
                        methods,
                        secondary_ctors: Vec::new(),
                        companion_methods: Vec::new(),
                        has_companion: false,
                        super_class,
                        interfaces: iface_names,
                        is_open: false,
                        is_abstract: false,
                        is_inner: false,
                        enum_entries,
                        annotations: Vec::new(),
                        has_type_params: false,
                        has_init_blocks: false,
                    };
                    register_class_in_table(&mut table, &simple_name, ext, diags);
                }
                Decl::Interface(iface) => {
                    let simple_name = interner.resolve(iface.name).to_string();
                    let methods: Vec<ExternalMethod> = iface
                        .methods
                        .iter()
                        .map(|m| ext_method_from_fun(m, interner, imports, aliases))
                        .collect();
                    let (_, iface_names) = build_supertypes(None, &iface.interfaces, interner);
                    let jvm_name = format!("{pkg_prefix}{simple_name}");
                    let ext = ExternalClassDecl {
                        jvm_name,
                        kind: ExternalClassKind::Interface,
                        fields: Vec::new(),
                        ctor_params: Vec::new(),
                        methods,
                        secondary_ctors: Vec::new(),
                        companion_methods: Vec::new(),
                        has_companion: false,
                        super_class: None,
                        interfaces: iface_names,
                        is_open: false,
                        is_abstract: true,
                        is_inner: false,
                        enum_entries: Vec::new(),
                        annotations: Vec::new(),
                        has_type_params: false,
                        has_init_blocks: false,
                    };
                    register_class_in_table(&mut table, &simple_name, ext, diags);
                }
                Decl::TypeAlias(ta) => {
                    let name = interner.resolve(ta.name).to_string();
                    table.type_aliases.insert(
                        name,
                        ExternalTypeAlias {
                            type_params: ta.type_params.clone(),
                            target: ta.target.clone(),
                        },
                    );
                }
                Decl::Unsupported { .. } => {}
            }
        }
    }

    table
}

/// Build a [`ResolvedFile`] from a parsed AST.
///
/// When `package_symbols` is `Some`, declarations from other files in the
/// same compilation unit are registered as [`DefId::ExternalPackage`] so
/// cross-file references resolve without error.
pub fn resolve_file(
    file: &KtFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    package_symbols: Option<&PackageSymbolTable>,
) -> ResolvedFile {
    let mut r = Resolver {
        interner,
        diags,
        out: ResolvedFile::default(),
    };
    let println_sym = r.interner.intern("println");
    r.out.top_level.insert(println_sym, DefId::PrintlnIntrinsic);
    let print_sym = r.interner.intern("print");
    r.out.top_level.insert(print_sym, DefId::PrintlnIntrinsic);
    // Register stdlib top-level functions — the canonical list lives
    // in `skotch_types::intrinsics::STDLIB_TOP_LEVEL_NAMES`.
    for name in skotch_types::intrinsics::STDLIB_TOP_LEVEL_NAMES {
        let sym = r.interner.intern(name);
        r.out.top_level.insert(sym, DefId::PrintlnIntrinsic); // reuse intrinsic DefId
    }

    // First pass: register every top-level fun/val by name so order doesn't matter.
    for (i, decl) in file.decls.iter().enumerate() {
        match decl {
            Decl::Fun(f) => {
                r.out.top_level.insert(f.name, DefId::Function(i as u32));
            }
            Decl::Val(v) => {
                r.out.top_level.insert(v.name, DefId::TopLevelVal(i as u32));
            }
            Decl::Class(c) => {
                r.out.top_level.insert(c.name, DefId::Function(i as u32));
            }
            Decl::Object(o) => {
                r.out
                    .top_level
                    .insert(o.name, DefId::PossibleExternal(o.name));
            }
            Decl::Enum(e) => {
                // Register enum name so Color.RED resolves.
                r.out
                    .top_level
                    .insert(e.name, DefId::PossibleExternal(e.name));
            }
            Decl::Interface(iface) => {
                r.out
                    .top_level
                    .insert(iface.name, DefId::PossibleExternal(iface.name));
            }
            Decl::TypeAlias(_) | Decl::Unsupported { .. } => {}
        }
    }

    // Register cross-file declarations from the PackageSymbolTable.
    // Only add entries that don't conflict with local declarations
    // (local declarations take priority).
    if let Some(pkg) = package_symbols {
        for name in pkg.functions.keys() {
            let sym = r.interner.intern(name);
            r.out
                .top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.vals.keys() {
            let sym = r.interner.intern(name);
            r.out
                .top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.classes.keys() {
            let sym = r.interner.intern(name);
            r.out
                .top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
    }

    // Register explicit (non-wildcard) imports under the bound name (alias
    // if present, else last path segment) as PossibleExternal so MIR
    // lowering can resolve them via the classinfo registry. Without this,
    // a lowercase imported function like `import androidx.compose.runtime.remember`
    // hits `is_possible_external("remember") == false` (starts lowercase,
    // not a known package prefix) and a spurious "unresolved identifier"
    // warning fires even though MIR lowering does resolve it later.
    for imp in &file.imports {
        if imp.is_wildcard {
            continue;
        }
        let name_sym = imp.alias.or_else(|| imp.path.last().copied());
        if let Some(sym) = name_sym {
            r.out
                .top_level
                .entry(sym)
                .or_insert(DefId::PossibleExternal(sym));
        }
    }

    // Second pass: resolve each decl's body.
    for (i, decl) in file.decls.iter().enumerate() {
        match decl {
            Decl::Fun(f) => {
                let rf = r.resolve_function(i as u32, f);
                r.out.functions.push(rf);
            }
            Decl::Val(v) => {
                let rv = r.resolve_top_val(v);
                r.out.top_vals.push(rv);
            }
            Decl::Class(_)
            | Decl::Object(_)
            | Decl::Enum(_)
            | Decl::Interface(_)
            | Decl::TypeAlias(_) => {}
            Decl::Unsupported { .. } => {}
        }
    }
    r.out
}

struct Resolver<'a> {
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    out: ResolvedFile,
}

impl<'a> Resolver<'a> {
    fn resolve_function(&mut self, idx: u32, f: &FunDecl) -> ResolvedFunction {
        let mut scope: Vec<(Symbol, DefId)> = Vec::new();
        // For extension functions: add `this` as the first parameter.
        if f.receiver_ty.is_some() {
            let this_sym = self.interner.intern("this");
            scope.push((this_sym, DefId::Param(idx, 0)));
        }
        let param_offset = if f.receiver_ty.is_some() { 1u32 } else { 0 };
        for (pi, p) in f.params.iter().enumerate() {
            scope.push((p.name, DefId::Param(idx, pi as u32 + param_offset)));
        }
        let mut rf = ResolvedFunction {
            name: f.name,
            params: f.params.iter().map(|p: &Param| p.name).collect(),
            locals: Vec::new(),
            body_refs: Vec::new(),
        };
        self.resolve_block(idx, &f.body, &mut scope, &mut rf);
        rf
    }

    fn resolve_top_val(&mut self, v: &ValDecl) -> ResolvedTopLevelVal {
        let mut refs = Vec::new();
        // Top-level vals don't have a function context; we still resolve
        // identifiers used in the initializer against the top-level scope.
        self.resolve_expr_in_top(&v.init, &mut refs);
        ResolvedTopLevelVal {
            name: v.name,
            init_refs: refs,
        }
    }

    fn resolve_block(
        &mut self,
        fn_idx: u32,
        block: &Block,
        scope: &mut Vec<(Symbol, DefId)>,
        rf: &mut ResolvedFunction,
    ) {
        let saved = scope.len();
        for stmt in &block.stmts {
            self.resolve_stmt(fn_idx, stmt, scope, rf);
        }
        scope.truncate(saved);
    }

    fn resolve_stmt(
        &mut self,
        fn_idx: u32,
        stmt: &Stmt,
        scope: &mut Vec<(Symbol, DefId)>,
        rf: &mut ResolvedFunction,
    ) {
        match stmt {
            Stmt::Expr(e) => self.resolve_expr(fn_idx, e, scope, rf),
            Stmt::Val(v) => {
                self.resolve_expr(fn_idx, &v.init, scope, rf);
                let local_idx = rf.locals.len() as u32;
                rf.locals.push(v.name);
                scope.push((v.name, DefId::Local(fn_idx, local_idx)));
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(fn_idx, v, scope, rf);
                }
            }
            Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
                self.resolve_expr(fn_idx, cond, scope, rf);
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
            }
            Stmt::For {
                var_name,
                start: range_start,
                end: range_end,
                step,
                body,
                ..
            } => {
                self.resolve_expr(fn_idx, range_start, scope, rf);
                self.resolve_expr(fn_idx, range_end, scope, rf);
                if let Some(step_e) = step {
                    self.resolve_expr(fn_idx, step_e, scope, rf);
                }
                // The loop variable is a new local.
                let local_idx = rf.locals.len() as u32;
                rf.locals.push(*var_name);
                scope.push((*var_name, DefId::Local(fn_idx, local_idx)));
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
            }
            Stmt::ForIn {
                var_name,
                destructure_names,
                iterable,
                body,
                ..
            } => {
                self.resolve_expr(fn_idx, iterable, scope, rf);
                if let Some(names) = destructure_names {
                    // Register each destructured name as a local.
                    for dn in names {
                        let local_idx = rf.locals.len() as u32;
                        rf.locals.push(*dn);
                        scope.push((*dn, DefId::Local(fn_idx, local_idx)));
                    }
                } else {
                    let local_idx = rf.locals.len() as u32;
                    rf.locals.push(*var_name);
                    scope.push((*var_name, DefId::Local(fn_idx, local_idx)));
                }
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
            }
            Stmt::Assign { target, value, .. } => {
                // Resolve the target — if not declared, it might be a
                // receiver scope property (e.g. `alpha = x` inside
                // `graphicsLayer { ... }`). Defer to MIR lowering.
                let _def = lookup(scope, *target).unwrap_or(DefId::Local(fn_idx, 0));
                self.resolve_expr(fn_idx, value, scope, rf);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::TryStmt {
                body,
                catch_param,
                catch_body,
                finally_body,
                ..
            } => {
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
                if let Some(cb) = catch_body {
                    // Push the catch parameter (e.g. `e` in `catch (e: Ex)`)
                    // into scope so it's resolvable in the catch body.
                    let saved = scope.len();
                    if let Some(param_sym) = catch_param {
                        // Use a dummy local index — the MIR lowerer creates
                        // the real local from the catch_param Symbol.
                        scope.push((*param_sym, DefId::Local(fn_idx, 9999)));
                    }
                    for s in &cb.stmts {
                        self.resolve_stmt(fn_idx, s, scope, rf);
                    }
                    scope.truncate(saved);
                }
                if let Some(fb) = finally_body {
                    for s in &fb.stmts {
                        self.resolve_stmt(fn_idx, s, scope, rf);
                    }
                }
            }
            Stmt::ThrowStmt { expr, .. } => {
                self.resolve_expr(fn_idx, expr, scope, rf);
            }
            Stmt::IndexAssign {
                receiver,
                index,
                value,
                ..
            } => {
                self.resolve_expr(fn_idx, receiver, scope, rf);
                self.resolve_expr(fn_idx, index, scope, rf);
                self.resolve_expr(fn_idx, value, scope, rf);
            }
            Stmt::FieldAssign {
                receiver, value, ..
            } => {
                self.resolve_expr(fn_idx, receiver, scope, rf);
                self.resolve_expr(fn_idx, value, scope, rf);
            }
            Stmt::Destructure { names, init, .. } => {
                self.resolve_expr(fn_idx, init, scope, rf);
                for name in names {
                    let local_idx = rf.locals.len() as u32;
                    rf.locals.push(*name);
                    scope.push((*name, DefId::Local(fn_idx, local_idx)));
                }
            }
            Stmt::LocalFun(f) => {
                // Register the local function name in scope so calls
                // to it resolve correctly. Use a synthetic Function DefId.
                // The actual index will be assigned during MIR lowering.
                self.out.top_level.insert(f.name, DefId::Function(999));
                // Resolve the function body with the OUTER scope visible
                // so the inner function can capture outer variables.
                let saved = scope.len();
                // Add the inner function's own parameters.
                let param_offset = if f.receiver_ty.is_some() { 1u32 } else { 0 };
                if f.receiver_ty.is_some() {
                    let this_sym = self.interner.intern("this");
                    scope.push((this_sym, DefId::Param(fn_idx, 0)));
                }
                for (pi, p) in f.params.iter().enumerate() {
                    scope.push((p.name, DefId::Param(fn_idx, pi as u32 + param_offset)));
                }
                self.resolve_block(fn_idx, &f.body, scope, rf);
                scope.truncate(saved);
            }
        }
    }

    fn resolve_expr(
        &mut self,
        fn_idx: u32,
        expr: &Expr,
        scope: &mut Vec<(Symbol, DefId)>,
        rf: &mut ResolvedFunction,
    ) {
        self.resolve_expr_ctx(fn_idx, expr, scope, rf, false);
    }

    fn resolve_expr_ctx(
        &mut self,
        fn_idx: u32,
        expr: &Expr,
        scope: &mut Vec<(Symbol, DefId)>,
        rf: &mut ResolvedFunction,
        is_callee: bool,
    ) {
        match expr {
            Expr::IntLit(_, _)
            | Expr::CharLit(_, _)
            | Expr::LongLit(_, _)
            | Expr::DoubleLit(_, _)
            | Expr::FloatLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::NullLit(_)
            | Expr::StringLit(_, _) => {}
            Expr::Lambda { params, body, .. } => {
                let saved = scope.len();
                for p in params {
                    scope.push((p.name, DefId::PossibleExternal(p.name)));
                }
                // If no explicit params, add implicit `it` to scope.
                if params.is_empty() {
                    let it_sym = self.interner.intern("it");
                    scope.push((it_sym, DefId::Local(fn_idx, 9998)));
                }
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
                scope.truncate(saved);
            }
            Expr::ObjectExpr { .. } => {
                // Object expression methods are resolved during MIR lowering.
            }
            Expr::Ident(name, span) => {
                let def = lookup(scope, *name).unwrap_or_else(|| {
                    self.out.top_level.get(name).copied().unwrap_or_else(|| {
                        let name_str = self.interner.resolve(*name);
                        // If the name could be an external class or package
                        // prefix (capitalized or known prefix), defer resolution
                        // to MIR lowering where the class registry lives.
                        if is_possible_external(name_str) {
                            return DefId::PossibleExternal(*name);
                        }
                        // Suppress the warning when this Ident is a
                        // call callee. The MIR-lower bare-Ident
                        // dispatch handles a wide menu of method-like
                        // resolutions the resolver doesn't see —
                        // implicit-this methods on the enclosing
                        // class, stdlib intrinsics on the extension
                        // receiver (e.g. `Int.shl`/`Int.ushr` from
                        // inside `Int.foo()`), companion-inherited
                        // members, etc. Warning here is noisy and
                        // every call would emit one even when the
                        // dispatch site below resolves cleanly.
                        if !is_callee {
                            self.diags.push(Diagnostic::warning(
                                *span,
                                format!("unresolved identifier `{name_str}`"),
                            ));
                        }
                        DefId::PossibleExternal(*name)
                    })
                });
                rf.body_refs.push(ResolvedRef { span: *span, def });
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr_ctx(fn_idx, callee, scope, rf, true);
                for a in args {
                    self.resolve_expr(fn_idx, &a.expr, scope, rf);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr(fn_idx, lhs, scope, rf);
                self.resolve_expr(fn_idx, rhs, scope, rf);
            }
            Expr::Unary { operand, .. } => self.resolve_expr(fn_idx, operand, scope, rf),
            Expr::Paren(inner, _) => self.resolve_expr(fn_idx, inner, scope, rf),
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.resolve_expr(fn_idx, cond, scope, rf);
                self.resolve_block(fn_idx, then_block, scope, rf);
                if let Some(eb) = else_block {
                    self.resolve_block(fn_idx, eb, scope, rf);
                }
            }
            Expr::Field { receiver, .. } | Expr::SafeCall { receiver, .. } => {
                self.resolve_expr(fn_idx, receiver, scope, rf);
            }
            Expr::Index {
                receiver, index, ..
            } => {
                self.resolve_expr(fn_idx, receiver, scope, rf);
                self.resolve_expr(fn_idx, index, scope, rf);
            }
            Expr::Throw { expr, .. }
            | Expr::NotNullAssert { expr, .. }
            | Expr::IsCheck { expr, .. }
            | Expr::AsCast { expr, .. } => {
                self.resolve_expr(fn_idx, expr, scope, rf);
            }
            Expr::IncDec { target, span, .. } => {
                // Resolve `target` the same way a bare `Ident` would
                // — the postfix bump only fires after the read.
                let synth = Expr::Ident(*target, *span);
                self.resolve_expr(fn_idx, &synth, scope, rf);
            }
            Expr::ElvisOp { lhs, rhs, .. } => {
                self.resolve_expr(fn_idx, lhs, scope, rf);
                self.resolve_expr(fn_idx, rhs, scope, rf);
            }
            Expr::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                self.resolve_block(fn_idx, body, scope, rf);
                if let Some(cb) = catch_body {
                    self.resolve_block(fn_idx, cb, scope, rf);
                }
                if let Some(fb) = finally_body {
                    self.resolve_block(fn_idx, fb, scope, rf);
                }
            }
            Expr::When {
                subject,
                branches,
                else_body,
                ..
            } => {
                self.resolve_expr(fn_idx, subject, scope, rf);
                for b in branches {
                    self.resolve_expr(fn_idx, &b.pattern, scope, rf);
                    self.resolve_expr(fn_idx, &b.body, scope, rf);
                }
                if let Some(eb) = else_body {
                    self.resolve_expr(fn_idx, eb, scope, rf);
                }
            }
            Expr::StringTemplate(parts, _) => {
                for p in parts {
                    match p {
                        TemplatePart::Text(_, _) => {}
                        TemplatePart::IdentRef(name, span) => {
                            let def = lookup(scope, *name).unwrap_or_else(|| {
                                self.out.top_level.get(name).copied().unwrap_or_else(|| {
                                    self.diags.push(Diagnostic::error(
                                        *span,
                                        format!(
                                            "unresolved identifier `{}` in string template",
                                            self.interner.resolve(*name)
                                        ),
                                    ));
                                    DefId::Error
                                })
                            });
                            rf.body_refs.push(ResolvedRef { span: *span, def });
                        }
                        TemplatePart::Expr(e) => self.resolve_expr(fn_idx, e, scope, rf),
                    }
                }
            }
        }
    }

    fn resolve_expr_in_top(&mut self, expr: &Expr, refs: &mut Vec<ResolvedRef>) {
        match expr {
            Expr::IntLit(_, _)
            | Expr::CharLit(_, _)
            | Expr::LongLit(_, _)
            | Expr::DoubleLit(_, _)
            | Expr::FloatLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::NullLit(_)
            | Expr::StringLit(_, _) => {}
            Expr::Ident(name, span) => {
                // If the identifier is defined at file-level, record the reference.
                // Otherwise, don't error here — it may be an imported type that
                // will be resolved during MIR lowering (via the import_map).
                let def = self
                    .out
                    .top_level
                    .get(name)
                    .copied()
                    .unwrap_or(DefId::Error);
                refs.push(ResolvedRef { span: *span, def });
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr_in_top(callee, refs);
                for a in args {
                    self.resolve_expr_in_top(&a.expr, refs);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.resolve_expr_in_top(lhs, refs);
                self.resolve_expr_in_top(rhs, refs);
            }
            Expr::Unary { operand, .. } => self.resolve_expr_in_top(operand, refs),
            Expr::Paren(inner, _) => self.resolve_expr_in_top(inner, refs),
            Expr::Field { receiver, .. } => self.resolve_expr_in_top(receiver, refs),
            Expr::When { .. } => {} // not supported in top-level initializers
            Expr::If { .. } => {
                // We don't yet lower if-expressions inside top-level
                // initializers; we'd need to materialize a <clinit>
                // basic block. Punt with a clear diagnostic.
                self.diags.push(Diagnostic::error(
                    expr.span(),
                    "if-expressions in top-level val initializers are not yet supported",
                ));
            }
            Expr::StringTemplate(parts, _) => {
                // Resolve ident refs inside the template.
                for p in parts {
                    if let TemplatePart::Expr(e) = p {
                        self.resolve_expr_in_top(e, refs);
                    }
                }
            }
            // New expression types — not meaningful at top level, just ignore.
            Expr::Throw { .. }
            | Expr::Try { .. }
            | Expr::ElvisOp { .. }
            | Expr::SafeCall { .. }
            | Expr::IsCheck { .. }
            | Expr::AsCast { .. }
            | Expr::NotNullAssert { .. }
            | Expr::IncDec { .. }
            | Expr::Lambda { .. }
            | Expr::ObjectExpr { .. }
            | Expr::Index { .. } => {}
        }
    }
}

fn lookup(scope: &[(Symbol, DefId)], name: Symbol) -> Option<DefId> {
    for (n, def) in scope.iter().rev() {
        if *n == name {
            return Some(*def);
        }
    }
    None
}
/// Check if a name matches a known Java class or package prefix.
/// Check if an unresolved name could be an external class or package.
///
/// Returns true for:
/// - Capitalized names (potential Java/Kotlin class: `System`, `Math`)
/// - Known package prefixes (for FQN chains: `java.lang.System`)
///
/// These are deferred to MIR lowering for resolution against the class
/// registry. If they don't resolve there either, MIR lowering emits a
/// clear "class not found on classpath" error.
fn is_possible_external(name: &str) -> bool {
    // Known Java/Kotlin package prefixes for fully-qualified names.
    if matches!(
        name,
        "java" | "javax" | "kotlin" | "org" | "com" | "io" | "net" | "android"
    ) {
        return true;
    }
    // Capitalized names are potential class references.
    name.starts_with(|c: char| c.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_lexer::lex;
    use skotch_parser::parse_file;
    use skotch_span::FileId;

    fn run(src: &str) -> (ResolvedFile, Diagnostics, Interner) {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let file = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&file, &mut interner, &mut diags, None);
        (r, diags, interner)
    }

    #[test]
    fn resolve_println_intrinsic() {
        let (r, d, _) = run(r#"fun main() { println("hi") }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(r.functions.len(), 1);
        let f = &r.functions[0];
        // Body should have one resolved ref: the `println` ident.
        assert_eq!(f.body_refs.len(), 1);
        assert_eq!(f.body_refs[0].def, DefId::PrintlnIntrinsic);
    }

    #[test]
    fn resolve_local_val() {
        let (r, d, _) = run(r#"fun main() { val s = "hi"; println(s) }"#);
        assert!(d.is_empty(), "{:?}", d);
        let f = &r.functions[0];
        // Two refs: `println` and `s`.
        assert_eq!(f.body_refs.len(), 2);
        assert_eq!(f.body_refs[0].def, DefId::PrintlnIntrinsic);
        assert_eq!(f.body_refs[1].def, DefId::Local(0, 0));
    }

    #[test]
    fn resolve_top_level_function_call() {
        let src = r#"
            fun greet(n: String) { println(n) }
            fun main() { greet("Kotlin") }
        "#;
        let (r, d, _) = run(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(r.functions.len(), 2);
        let main = &r.functions[1];
        // greet ref
        assert!(main
            .body_refs
            .iter()
            .any(|rr| matches!(rr.def, DefId::Function(0))));
    }

    #[test]
    fn resolve_top_level_val() {
        let (r, d, _) = run(r#"val GREETING = "hi"; fun main() { println(GREETING) }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(r.top_vals.len(), 1);
        let main = &r.functions[0];
        assert!(main
            .body_refs
            .iter()
            .any(|rr| matches!(rr.def, DefId::TopLevelVal(0))));
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: resolve_class_member       — Foo.bar() once classes land
    // TODO: resolve_import_alias       — `import x.y.Z as Q`
    // TODO: resolve_extension_function — `fun String.shout()`
    // TODO: error_unresolved_ident     — `println(undefined)`
    // TODO: error_shadowed_local       — outer val shadowed by inner
}
