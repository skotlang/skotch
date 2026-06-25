//! Name resolution: shared types + the typed (SIL-backed) pass.
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
use skotch_intern::Symbol;
use skotch_span::Span;
// (legacy `use skotch_syntax::{...}` removed; kept Annotation/Param/Stmt etc were only used by the legacy resolve_file impl which is gone now)
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
    /// Secondary index from simple class name → FQ class name. The
    /// primary `classes` map is keyed by FQ name to disambiguate
    /// classes with the same simple name in different sub-packages.
    /// Most call sites look up by simple name and resolve through this
    /// index. Collisions emit a diagnostic from `gather_declarations`.
    #[serde(default)]
    pub simple_name_to_fq: std::collections::HashMap<String, String>,
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
    /// Per-param lambda receiver class for `T.() -> R` (extension
    /// function type) params. Same length as `param_tys` when present;
    /// `None` for non-receiver-typed params. Lets cross-file DSL
    /// builder call sites (`html { body { ... } }` where `html` and
    /// `body` live in another file) install the implicit receiver so
    /// the trailing lambda body's bare member calls dispatch via
    /// `recv.method(...)`. In-file fns get this via
    /// `fn_param_lambda_receiver_class` in mir-lower; this surfaces
    /// the same info across the resolve boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub param_receiver_classes: Vec<Option<String>>,
    /// Annotation simple-names on this function (`"JvmStatic"`,
    /// `"Composable"`, `"Deprecated"`, etc).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
    /// Phase H5c cross-file metadata: when this is a top-level extension
    /// function whose receiver type is a `@JvmInline value class V(val u:
    /// U)`, this carries `(value_class_jvm_name, underlying_ty)` — the
    /// same shape as
    /// [`skotch_mir::MirFunction::is_value_class_extension`]. Cross-file
    /// callers in another file consult this so the call-site rewrite
    /// can emit the kotlinc-shaped erased static dispatch (mangled
    /// `<name>-<KEEP104-mangle>` + underlying-typed slot 0) against the
    /// declaring file's facade class. `None` when the receiver is not a
    /// value class. Mirrors H4's `ExternalClassDecl::is_value_class` +
    /// `value_underlying_ty` cross-file plumbing, but on the function
    /// side instead of the class side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_value_class_extension: Option<(String, Ty)>,
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
    /// Phase H4 cross-file metadata: true when the source declaration
    /// carried both `@JvmInline` and the soft `value` modifier (the
    /// Kotlin 1.5+ inline value-class shape). Consumers in other files
    /// need this to route constructor calls through `box-impl` and
    /// method calls through the static `<name>[-mangle]-impl` variants
    /// instead of `invokevirtual` against the wrapper. Defaults to
    /// false so existing fixtures are byte-stable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_value_class: bool,
    /// Phase H4 cross-file metadata: when [`Self::is_value_class`] is
    /// true, this is the `Ty` of the single primary-ctor `val`
    /// parameter (e.g. `Ty::Long` for `value class UserId(val raw:
    /// Long)`). Consumers use it to build the erased descriptors
    /// (`(J)LUserId;` for `box-impl`, `(J)J` for `doubled-impl`).
    /// `None` when the class is not a value class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_underlying_ty: Option<Ty>,
    /// Compile-time-known `const val` declarations on this class's
    /// companion object, paired with their literal value. Kotlin
    /// compiles a `companion object { const val NAME = LITERAL }`
    /// shape as a `static final` field on the OUTER class with a
    /// `ConstantValue` attribute, and constant-folds bare references
    /// to `NAME` at every use site. Subclasses can refer to inherited
    /// `const val`s by simple name in their bodies — the cross-file
    /// caller (mir-lower's super-ctor delegation walker) walks the
    /// super-class chain in this table and inlines the matching value
    /// at the call site, the same way kotlinc does. Empty for kinds
    /// with no companion (object, enum, interface) and for companions
    /// that hold no `const val` decls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companion_const_inits: Vec<(String, CompanionConstValue)>,
}

/// Compile-time value of a `const val` declared inside a class's
/// companion object. Encodes the literal type so cross-file consumers
/// can pick the right `MirConst` variant and JVM descriptor letter
/// (e.g. `B` vs `I`) without re-parsing the source text. Mirrors the
/// shape of `MirConst` from `skotch-mir` without pulling in that
/// dependency — `skotch-resolve` sits below `mir-lower` in the dep
/// graph.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum CompanionConstValue {
    String(String),
    Int(i32),
    Long(i64),
    Bool(bool),
    /// Byte-typed `const val` — JVM has no Byte primitive at the
    /// operand-stack level, but the descriptor letter (`B`) matters
    /// at the call site so we carry the declared-type tag here.
    Byte(i8),
    Char(char),
    Float(f32),
    Double(f64),
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
