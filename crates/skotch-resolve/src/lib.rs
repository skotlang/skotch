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
    Block, Decl, Expr, FunDecl, KtFile, Param, Stmt, TemplatePart, TypeRef, ValDecl, Visibility,
};
use skotch_types::Ty;

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
    /// User-defined class/object/enum/interface: name → declaration metadata.
    pub classes: std::collections::HashMap<String, ExternalClassDecl>,
    /// `typealias` declarations: simple-name → AST `TypeRef` of the
    /// alias target. Surfaced so typeck in another file can resolve
    /// the alias to its underlying type (e.g.
    /// `typealias Predicate = (Int) -> Boolean` → caller sees
    /// `Ty::Function`). Not serialized — `TypeRef` doesn't impl
    /// serde traits and the table rebuilds each compile.
    #[serde(skip)]
    pub type_aliases: std::collections::HashMap<String, skotch_syntax::TypeRef>,
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
    /// True if this is an extension function.
    pub is_extension: bool,
}

/// Metadata for a top-level val from another file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExternalValDecl {
    pub owner_class: String,
    pub ty: Ty,
}

/// Metadata for a class/object/enum/interface from another file.
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
    pub ctor_params: Vec<(String, Ty)>,
    /// Method signatures: (name, param_tys, return_ty).
    pub methods: Vec<(String, Vec<Ty>, Ty)>,
    /// Companion-object method signatures, used so call sites that go
    /// through `OuterClass.method(...)` can resolve to the companion's
    /// virtual method via `OuterClass$Companion`. Empty when the class
    /// has no `companion object`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companion_methods: Vec<(String, Vec<Ty>, Ty)>,
    /// Superclass name (simple, not FQ).
    pub super_class: Option<String>,
    /// Interface names this class/object implements (simple, not FQ).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,
    /// Whether the class is open.
    pub is_open: bool,
    /// Whether the class is abstract.
    pub is_abstract: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ExternalClassKind {
    Class,
    DataClass,
    Object,
    Enum,
    Interface,
    SealedClass,
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

/// Gather all top-level declarations from multiple parsed files into a
/// shared [`PackageSymbolTable`]. This is Phase 1 of multi-file compilation.
///
/// Each entry in `files` is `(file_id, parsed_ast, wrapper_class_name)`.
pub fn gather_declarations(
    files: &[(FileId, &KtFile, &str)],
    interner: &Interner,
) -> PackageSymbolTable {
    let mut table = PackageSymbolTable::default();

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
                        // Insert Composer + Int before the closing ')'.
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
                        f.params.len() + 2 // +$composer +$changed
                    } else {
                        f.params.len()
                    };
                    let ext = ExternalFunDecl {
                        owner_class: fq_wrapper.clone(),
                        descriptor,
                        return_ty,
                        param_count,
                        param_tys,
                        is_suspend: f.is_suspend,
                        is_extension: f.receiver_ty.is_some(),
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
                        },
                    );
                }
                Decl::Class(c) => {
                    if c.visibility == Visibility::Private {
                        continue;
                    }
                    let name = interner.resolve(c.name).to_string();
                    let kind = if c.is_data {
                        ExternalClassKind::DataClass
                    } else {
                        ExternalClassKind::Class
                    };
                    // Collect constructor param fields (val/var).
                    let fields: Vec<(String, Ty)> = c
                        .constructor_params
                        .iter()
                        .filter(|p| p.is_val || p.is_var)
                        .map(|p| {
                            let fname = interner.resolve(p.name).to_string();
                            let fty =
                                type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases);
                            (fname, fty)
                        })
                        .collect();
                    // ALL primary-constructor params in declaration order
                    // (val/var or plain) — needed cross-file to (a) build
                    // a full `<init>` descriptor and (b) reorder named
                    // args to positional. `fields` is a subset.
                    let ctor_params: Vec<(String, Ty)> = c
                        .constructor_params
                        .iter()
                        .map(|p| {
                            let pname = interner.resolve(p.name).to_string();
                            let pty =
                                type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases);
                            (pname, pty)
                        })
                        .collect();
                    // Collect method signatures. Also surface synthetic
                    // `getXxx()` accessors for each `val/var` body
                    // property so cross-file callers can dispatch
                    // through `Foo.x` as `invokevirtual getX()`
                    // (without these, the Field-lowering falls back
                    // to a `getfield x:Object` with the wrong
                    // descriptor — `NoSuchFieldError` at runtime).
                    let property_getters: Vec<(String, Vec<Ty>, Ty)> = c
                        .properties
                        .iter()
                        .filter_map(|p| {
                            let pname = interner.resolve(p.name).to_string();
                            // `@JvmField` properties are exposed as bare
                            // fields with no synthesized getter, so skip.
                            let is_jvm_field = p.annotations.iter().any(|a| {
                                let n = interner.resolve(a.name);
                                n == "JvmField" || n == "kotlin/jvm/JvmField"
                            });
                            if is_jvm_field {
                                return None;
                            }
                            let pty =
                                p.ty.as_ref()
                                    .map(|tr| {
                                        type_ref_to_ty_with_aliases(tr, interner, imports, aliases)
                                    })
                                    .unwrap_or(Ty::Any);
                            let mut first_char = pname.chars();
                            let getter_name = match first_char.next() {
                                Some(c) => format!(
                                    "get{}{}",
                                    c.to_uppercase().collect::<String>(),
                                    first_char.as_str()
                                ),
                                None => return None,
                            };
                            Some((getter_name, Vec::new(), pty))
                        })
                        .collect();
                    let mut methods: Vec<(String, Vec<Ty>, Ty)> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let mname = interner.resolve(m.name).to_string();
                            let ptys: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| {
                                    type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases)
                                })
                                .collect();
                            let rty = m
                                .return_ty
                                .as_ref()
                                .map(|tr| {
                                    type_ref_to_ty_with_aliases(tr, interner, imports, aliases)
                                })
                                .unwrap_or_else(|| infer_body_return_ty(&m.body, interner));
                            (mname, ptys, rty)
                        })
                        .collect();
                    methods.extend(property_getters);
                    // Companion-object methods are invisible cross-file
                    // unless we surface them here. Call sites like
                    // `OuterClass.method(...)` resolve via the companion
                    // dispatch in mir-lower, which needs to know the
                    // companion class exists.
                    let companion_methods: Vec<(String, Vec<Ty>, Ty)> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mname = interner.resolve(m.name).to_string();
                            let ptys: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| {
                                    type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases)
                                })
                                .collect();
                            let rty = m
                                .return_ty
                                .as_ref()
                                .map(|tr| {
                                    type_ref_to_ty_with_aliases(tr, interner, imports, aliases)
                                })
                                .unwrap_or_else(|| infer_body_return_ty(&m.body, interner));
                            (mname, ptys, rty)
                        })
                        .collect();
                    let super_class = c
                        .parent_class
                        .as_ref()
                        .map(|sc| interner.resolve(sc.name).to_string());
                    let interfaces = c
                        .interfaces
                        .iter()
                        .map(|s| interner.resolve(*s).to_string())
                        .collect();
                    table.classes.insert(
                        name,
                        ExternalClassDecl {
                            jvm_name: format!("{pkg_prefix}{}", interner.resolve(c.name)),
                            kind,
                            fields,
                            ctor_params,
                            methods,
                            companion_methods,
                            super_class,
                            interfaces,
                            is_open: c.is_open,
                            is_abstract: c.is_abstract,
                        },
                    );
                }
                Decl::Object(o) => {
                    let name = interner.resolve(o.name).to_string();
                    let methods: Vec<(String, Vec<Ty>, Ty)> = o
                        .methods
                        .iter()
                        .map(|m| {
                            let mname = interner.resolve(m.name).to_string();
                            let ptys: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| {
                                    type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases)
                                })
                                .collect();
                            let rty = m
                                .return_ty
                                .as_ref()
                                .map(|tr| {
                                    type_ref_to_ty_with_aliases(tr, interner, imports, aliases)
                                })
                                .unwrap_or_else(|| infer_body_return_ty(&m.body, interner));
                            (mname, ptys, rty)
                        })
                        .collect();
                    let interfaces = o
                        .interfaces
                        .iter()
                        .map(|s| interner.resolve(*s).to_string())
                        .collect();
                    table.classes.insert(
                        name,
                        ExternalClassDecl {
                            jvm_name: format!("{pkg_prefix}{}", interner.resolve(o.name)),
                            kind: ExternalClassKind::Object,
                            fields: Vec::new(),
                            ctor_params: Vec::new(),
                            methods,
                            companion_methods: Vec::new(),
                            super_class: None,
                            interfaces,
                            is_open: false,
                            is_abstract: false,
                        },
                    );
                }
                Decl::Enum(e) => {
                    let name = interner.resolve(e.name).to_string();
                    table.classes.insert(
                        name,
                        ExternalClassDecl {
                            jvm_name: format!("{pkg_prefix}{}", interner.resolve(e.name)),
                            kind: ExternalClassKind::Enum,
                            fields: Vec::new(),
                            ctor_params: Vec::new(),
                            methods: Vec::new(),
                            companion_methods: Vec::new(),
                            super_class: None,
                            interfaces: Vec::new(),
                            is_open: false,
                            is_abstract: false,
                        },
                    );
                }
                Decl::Interface(iface) => {
                    let name = interner.resolve(iface.name).to_string();
                    let methods: Vec<(String, Vec<Ty>, Ty)> = iface
                        .methods
                        .iter()
                        .map(|m| {
                            let mname = interner.resolve(m.name).to_string();
                            let ptys: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| {
                                    type_ref_to_ty_with_aliases(&p.ty, interner, imports, aliases)
                                })
                                .collect();
                            let rty = m
                                .return_ty
                                .as_ref()
                                .map(|tr| {
                                    type_ref_to_ty_with_aliases(tr, interner, imports, aliases)
                                })
                                .unwrap_or_else(|| infer_body_return_ty(&m.body, interner));
                            (mname, ptys, rty)
                        })
                        .collect();
                    table.classes.insert(
                        name,
                        ExternalClassDecl {
                            jvm_name: format!("{pkg_prefix}{}", interner.resolve(iface.name)),
                            kind: ExternalClassKind::Interface,
                            fields: Vec::new(),
                            ctor_params: Vec::new(),
                            methods,
                            companion_methods: Vec::new(),
                            super_class: None,
                            interfaces: Vec::new(),
                            is_open: false,
                            is_abstract: true,
                        },
                    );
                }
                Decl::TypeAlias(ta) => {
                    let name = interner.resolve(ta.name).to_string();
                    table.type_aliases.insert(name, ta.target.clone());
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
        _is_callee: bool,
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
                        // Emit diagnostic but defer to MIR lowering via
                        // PossibleExternal (not Error). MIR lowering has more
                        // context (classpath, import map, lambda receiver scope)
                        // to resolve identifiers. Using PossibleExternal instead
                        // of Error avoids triggering `has_null_stubs()` stubbing.
                        // Use warning level: MIR lowering may still resolve it.
                        self.diags.push(Diagnostic::warning(
                            *span,
                            format!("unresolved identifier `{name_str}`"),
                        ));
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
