//! Typed-AST entry points for name resolution.
//!
//! Mirrors [`crate`]'s legacy entry points (`gather_declarations`,
//! `resolve_file`) but consumes [`skotch_ast::KtFile`] (typed wrapper
//! over the SIL tree) instead of `&skotch_syntax::KtFile` (legacy
//! Box-tree AST).
//!
//! The intent is byte-for-byte parity with the legacy implementation
//! at the [`PackageSymbolTable`] / [`ResolvedFile`] level. Once that
//! parity is verified end-to-end, the legacy entry points become
//! shims that re-parse to SIL and call into this module.
//!
//! ## Migration coverage
//!
//! Done in this module:
//! - Top-level fun/val/class/interface/object/enum/typealias gathering
//! - Per-file import map, same-package decl visibility
//! - TypeRef → JVM descriptor (with typealias substitution, function
//!   types, generics, nullable, kotlin→java collection erasure)
//! - TypeRef → `Ty` (typechecker shape)
//! - Cross-file class registration with `Outer$Inner` JVM names
//! - Property getter synthesis (`getX()`), suppressed for `@JvmField`
//! - Companion / secondary-constructor / supertype propagation
//! - Annotation short-name collection
//! - Stdlib top-level intrinsic registration
//!
//! Not yet covered (next migration sessions):
//! - Body-level reference resolution (the `Resolver` impl on legacy)
//! - Smart-cast `when` arm scopes
//! - Local function nesting

use rustc_hash::FxHashMap;
use skotch_ast::{
    children, AstNode, KtClass, KtClassBody, KtDecl, KtEnumClass, KtEnumEntry, KtFile, KtFun,
    KtFunctionType, KtInterface, KtObjectDeclaration, KtPrimaryConstructor, KtProperty,
    KtSecondaryConstructor, KtTypeReference, KtUserType, KtValueParameter, KtValueParameterList,
};
use skotch_intern::{Interner, Symbol};
use skotch_syntax::{SyntaxKind, Visibility};
use skotch_types::Ty;

use crate::{
    DefId, ExternalClassDecl, ExternalClassKind, ExternalConstructor, ExternalFunDecl,
    ExternalMethod, ExternalParam, ExternalValDecl, PackageSymbolTable, ResolvedFile,
    ResolvedFunction,
};

// ── TypeRef → JVM descriptor ────────────────────────────────────────

/// Build the JVM descriptor string for a typed `KtTypeReference`,
/// applying typealias substitution and import resolution.
fn type_ref_to_descriptor(
    tr: KtTypeReference<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    param_position: bool,
) -> String {
    // Function type: `(P, ...) -> R` → `Lkotlin/jvm/functions/FunctionN;`
    if let Some(ft) = tr.function_type() {
        let base = ft.parameter_list().map_or(0, |pl| pl.parameters().count());
        let arity =
            base + if tr.is_composable() { 2 } else { 0 } + if tr.is_suspend() { 1 } else { 0 };
        return format!("Lkotlin/jvm/functions/Function{arity};");
    }
    // Nullable wrapper — for typecheck-bound nullable types, fall back to
    // `Ljava/lang/Object;` (legacy parity).
    if let Some(n) = tr.nullable_type() {
        // For a function type wrapped in nullable, expand the same way
        // so callers see the Function shape (matches legacy).
        if let Some(ft) = n.inner_function_type() {
            let base = ft.parameter_list().map_or(0, |pl| pl.parameters().count());
            let arity =
                base + if tr.is_composable() { 2 } else { 0 } + if tr.is_suspend() { 1 } else { 0 };
            return format!("Lkotlin/jvm/functions/Function{arity};");
        }
        return "Ljava/lang/Object;".to_string();
    }

    let user = match tr.user_type() {
        Some(u) => u,
        None => return "Ljava/lang/Object;".to_string(),
    };
    let name = user.name().unwrap_or("");
    // Typealias substitution — `typealias Predicate = (Int) -> Boolean`
    if let Some(target) = aliases.get(name) {
        return alias_target_to_descriptor(target, imports, aliases, param_position);
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
            if let Some(fq) = imports.get(name) {
                format!("L{fq};")
            } else if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
                format!("L{jvm};")
            } else {
                "Ljava/lang/Object;".to_string()
            }
        }
    }
}

/// Build the JVM descriptor for a method: `(params)return`.
fn build_method_descriptor(
    params: Option<KtValueParameterList<'_>>,
    return_type: Option<KtTypeReference<'_>>,
    receiver_type: Option<KtTypeReference<'_>>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> String {
    let mut desc = String::from("(");
    if let Some(rt) = receiver_type {
        desc.push_str(&type_ref_to_descriptor(rt, imports, aliases, true));
    }
    if let Some(plist) = params {
        for p in plist.parameters() {
            if let Some(ty) = p.type_reference() {
                desc.push_str(&type_ref_to_descriptor(ty, imports, aliases, true));
            } else {
                desc.push_str("Ljava/lang/Object;");
            }
        }
    }
    desc.push(')');
    if let Some(rt) = return_type {
        desc.push_str(&type_ref_to_descriptor(rt, imports, aliases, false));
    } else {
        desc.push('V');
    }
    desc
}

// ── TypeRef → Ty ────────────────────────────────────────────────────

fn type_ref_to_ty(
    tr: KtTypeReference<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    if let Some(ft) = tr.function_type() {
        let params: Vec<Ty> = ft
            .parameter_list()
            .map(|pl| {
                pl.parameters()
                    .map(|p| {
                        p.type_reference()
                            .map(|ptr| type_ref_to_ty(ptr, imports, aliases))
                            .unwrap_or(Ty::Any)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ret = ft
            .return_type()
            .map(|rtr| type_ref_to_ty(rtr, imports, aliases))
            .unwrap_or(Ty::Unit);
        let base = Ty::Function {
            params,
            ret: Box::new(ret),
            is_suspend: tr.is_suspend(),
            is_composable: tr.is_composable(),
        };
        return base;
    }
    if let Some(n) = tr.nullable_type() {
        let inner = if let Some(u) = n.inner_user_type() {
            user_type_to_ty(u, imports, aliases)
        } else if let Some(ft) = n.inner_function_type() {
            // Reconstruct function type Ty by inspecting the inner
            // FUNCTION_TYPE shape (no easy wrapper here).
            function_type_to_ty(ft, imports, aliases, false, false)
        } else {
            Ty::Any
        };
        return Ty::Nullable(Box::new(inner));
    }
    if let Some(u) = tr.user_type() {
        return user_type_to_ty(u, imports, aliases);
    }
    Ty::Any
}

fn user_type_to_ty(
    u: KtUserType<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    let name = u.name().unwrap_or("");
    if let Some(target) = aliases.get(name) {
        return alias_target_to_ty(target, imports, aliases);
    }
    skotch_types::ty_from_name(name).unwrap_or_else(|| {
        if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
            Ty::Class(jvm.to_string())
        } else if let Some(fq) = imports.get(name) {
            Ty::Class(fq.clone())
        } else {
            Ty::Any
        }
    })
}

fn function_type_to_ty(
    ft: KtFunctionType<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    is_suspend: bool,
    is_composable: bool,
) -> Ty {
    let params: Vec<Ty> = ft
        .parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| {
                    p.type_reference()
                        .map(|ptr| type_ref_to_ty(ptr, imports, aliases))
                        .unwrap_or(Ty::Any)
                })
                .collect()
        })
        .unwrap_or_default();
    let ret = ft
        .return_type()
        .map(|rtr| type_ref_to_ty(rtr, imports, aliases))
        .unwrap_or(Ty::Unit);
    Ty::Function {
        params,
        ret: Box::new(ret),
        is_suspend,
        is_composable,
    }
}

// ── Type alias target ────────────────────────────────────────────────

/// What a `typealias` resolves to. For descriptor/Ty substitution we
/// need to be able to re-walk the alias target as a TypeRef shape. We
/// cache the raw SilNode of the target TYPE_REFERENCE composite so the
/// substitution walk re-enters [`type_ref_to_descriptor`] /
/// [`type_ref_to_ty`].
#[derive(Clone)]
struct AliasTarget {
    /// A pointer to the underlying SIL node. Lifetime-erased — safe so
    /// long as the [`skotch_ast::ParsedFile`] remains pinned during the
    /// gather/resolve pass.
    target_node_ptr: usize,
}

impl AliasTarget {
    fn from_type_ref(tr: KtTypeReference<'_>) -> Self {
        Self {
            target_node_ptr: tr.syntax() as *const _ as usize,
        }
    }
    fn as_type_ref<'a>(&self) -> KtTypeReference<'a> {
        // Safety: the SilNode pointer is non-null and valid for the
        // lifetime of the enclosing ParsedFile; the gather pass holds
        // the tree pinned via the &[(KtFile,&str)] slice.
        let raw = self.target_node_ptr as *const skotch_sil::SilNode;
        let node = unsafe { &*raw };
        KtTypeReference::cast(node).expect("alias target stored as TYPE_REFERENCE")
    }
}

fn alias_target_to_descriptor(
    t: &AliasTarget,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    param_position: bool,
) -> String {
    type_ref_to_descriptor(t.as_type_ref(), imports, aliases, param_position)
}

fn alias_target_to_ty(
    t: &AliasTarget,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    type_ref_to_ty(t.as_type_ref(), imports, aliases)
}

// ── External-param / external-method builders ───────────────────────

fn ext_param_from_value_parameter(
    p: KtValueParameter<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> ExternalParam {
    let ty = p
        .type_reference()
        .map(|tr| type_ref_to_ty(tr, imports, aliases))
        .unwrap_or(Ty::Any);
    // Receiver class for `(Foo.() -> R)` lambda parameter types.
    let receiver_class = p.type_reference().and_then(|tr| {
        tr.function_type()
            .and_then(|ft| ft.receiver())
            .and_then(|r| r.type_reference())
            .and_then(|rt| rt.user_type())
            .and_then(|u| u.name().map(|s| s.to_string()))
    });
    ExternalParam {
        name: p.name().unwrap_or("").to_string(),
        ty,
        has_default: p.default_value().is_some(),
        is_vararg: p.is_vararg(),
        receiver_class,
    }
}

fn ext_method_from_fun(
    f: KtFun<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> ExternalMethod {
    let params: Vec<ExternalParam> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| ext_param_from_value_parameter(p, imports, aliases))
                .collect()
        })
        .unwrap_or_default();
    let return_ty = f
        .return_type()
        .map(|tr| type_ref_to_ty(tr, imports, aliases))
        .unwrap_or_else(|| infer_body_return_ty(f));
    let receiver_ty = f
        .receiver_type()
        .map(|tr| type_ref_to_ty(tr, imports, aliases));
    ExternalMethod {
        name: f.name().unwrap_or("").to_string(),
        params,
        return_ty,
        is_suspend: f.is_suspend(),
        is_inline: f.is_inline(),
        is_abstract: f.is_abstract(),
        is_open: f.is_open(),
        receiver_ty,
        annotations: f.annotation_names().into_iter().map(String::from).collect(),
    }
}

/// Best-effort return-type inference from the function body. Mirrors
/// the legacy `infer_body_return_ty` semantics: only when an explicit
/// `return value` statement is present we infer the type of the
/// returned expression; otherwise `Unit`. Currently a stub returning
/// `Unit` — typed-AST body walking lands in a follow-up.
fn infer_body_return_ty(f: KtFun<'_>) -> Ty {
    use skotch_ast::KtExpr;
    let block = match f.body_block() {
        Some(b) => b,
        None => {
            // No body and no expression body either → abstract/
            // interface method. Kotlin defaults the return type to
            // Unit, not Any. Returning Any here caused cross-file
            // callsites against interface methods to emit
            // `()Ljava/lang/Object;` descriptors against the real
            // `()V` interface method → NoSuchMethodError at runtime.
            // Expression-body inference is still TODO; if a real
            // expression-body is later detected, it can fall through
            // to a smarter inference. Today, the only consumers of
            // this path are abstract iface/class methods, so Unit is
            // strictly more correct than Any.
            if f.body_expression().is_some() {
                return Ty::Any;
            }
            return Ty::Unit;
        }
    };
    // Scan statements in reverse for a `return value` expression.
    let mut returned: Option<KtExpr<'_>> = None;
    for stmt in block.statements() {
        if let KtExpr::Return(r) = stmt {
            // Find any non-trivia child expression of the return node.
            for c in children(r.syntax()) {
                if let Some(e) = KtExpr::cast(c) {
                    returned = Some(e);
                }
            }
        }
    }
    let Some(e) = returned else { return Ty::Unit };
    match e {
        KtExpr::Boolean(_) => Ty::Bool,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Float(_) => Ty::Double,
        KtExpr::Character(_) => Ty::Char,
        KtExpr::Null(_) => Ty::Nullable(Box::new(Ty::Any)),
        KtExpr::String(_) => Ty::String,
        KtExpr::Binary(b) => {
            // Boolean-producing binary ops (==, !=, <, >, &&, ||).
            let op = b.operation().map(|o| o.text()).unwrap_or_default();
            match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                _ => Ty::Any,
            }
        }
        _ => Ty::Any,
    }
}

fn property_getter_method(
    p: KtProperty<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Option<ExternalMethod> {
    let annots = p.annotation_names();
    let is_jvm_field = annots.contains(&"JvmField");
    if is_jvm_field {
        return None;
    }
    let pname = p.name()?;
    let mut chars = pname.chars();
    let getter_name = format!(
        "get{}{}",
        chars.next()?.to_uppercase().collect::<String>(),
        chars.as_str()
    );
    let ret = p
        .type_reference()
        .map(|tr| type_ref_to_ty(tr, imports, aliases))
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
        annotations: annots.into_iter().map(String::from).collect(),
    })
}

// ── Class shape ─────────────────────────────────────────────────────

fn build_ctor_shape(
    primary: Option<KtPrimaryConstructor<'_>>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> (Vec<(String, Ty)>, Vec<ExternalParam>) {
    let Some(pc) = primary else {
        return (Vec::new(), Vec::new());
    };
    let plist = match pc.value_parameter_list() {
        Some(pl) => pl,
        None => return (Vec::new(), Vec::new()),
    };
    let mut fields = Vec::new();
    let mut ctor_params = Vec::new();
    for p in plist.parameters() {
        let is_val_var = p.is_val() || p.is_var();
        let is_private = p
            .modifier_list()
            .map(|m| m.has_kind(SyntaxKind::KW_PRIVATE))
            .unwrap_or(false);
        let name = p.name().unwrap_or("").to_string();
        let ty = p
            .type_reference()
            .map(|tr| type_ref_to_ty(tr, imports, aliases))
            .unwrap_or(Ty::Any);
        if is_val_var && !is_private {
            fields.push((name.clone(), ty.clone()));
        }
        ctor_params.push(ExternalParam {
            name,
            ty,
            has_default: p.default_value().is_some(),
            is_vararg: p.is_vararg(),
            receiver_class: None,
        });
    }
    (fields, ctor_params)
}

fn collect_supertypes(
    class_super_type_list: Option<skotch_ast::KtSuperTypeList<'_>>,
) -> (Option<String>, Vec<String>) {
    let Some(list) = class_super_type_list else {
        return (None, Vec::new());
    };
    let mut super_class = None;
    let mut ifaces = Vec::new();
    for entry in list.entries() {
        let tr = entry.type_reference();
        let name = tr
            .and_then(|t| t.user_type())
            .and_then(|u| u.name())
            .map(|s| s.to_string());
        let is_call = matches!(entry, skotch_ast::SuperTypeEntry::Call(_));
        match name {
            Some(n) if is_call => {
                // SUPER_TYPE_CALL_ENTRY → this is the super-class invocation.
                super_class = Some(n);
            }
            Some(n) => {
                ifaces.push(n);
            }
            None => {}
        }
    }
    (super_class, ifaces)
}

fn body_methods_and_props<'a>(
    body: Option<KtClassBody<'a>>,
) -> (Vec<KtFun<'a>>, Vec<KtProperty<'a>>) {
    let mut methods = Vec::new();
    let mut props = Vec::new();
    if let Some(b) = body {
        for d in b.declarations() {
            match d {
                KtDecl::Fun(f) => methods.push(f),
                KtDecl::Property(p) => props.push(p),
                _ => {}
            }
        }
    }
    (methods, props)
}

fn companion_members<'a>(
    body: Option<KtClassBody<'a>>,
) -> (Vec<KtFun<'a>>, Vec<KtProperty<'a>>, bool) {
    let mut methods = Vec::new();
    let mut props = Vec::new();
    let mut has_companion = false;
    if let Some(b) = body {
        for d in b.declarations() {
            if let KtDecl::Object(o) = d {
                if o.is_companion() {
                    has_companion = true;
                    let (m, p) = body_methods_and_props(o.body());
                    methods.extend(m);
                    props.extend(p);
                }
            }
        }
    }
    (methods, props, has_companion)
}

fn nested_classes<'a>(body: Option<KtClassBody<'a>>) -> Vec<KtClass<'a>> {
    let mut out = Vec::new();
    if let Some(b) = body {
        for d in b.declarations() {
            if let KtDecl::Class(c) = d {
                out.push(c);
            }
        }
    }
    out
}

fn gather_class_recursive(
    c: KtClass<'_>,
    fq_outer: &str,
    table: &mut PackageSymbolTable,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    if c.visibility() == Visibility::Private {
        return;
    }
    let simple_name = c.name().unwrap_or("").to_string();
    if simple_name.is_empty() {
        return;
    }
    let kind = if c.is_sealed() {
        ExternalClassKind::SealedClass
    } else if c.is_data() {
        ExternalClassKind::DataClass
    } else {
        ExternalClassKind::Class
    };
    let jvm_name_pre = format!("{fq_outer}{simple_name}");

    // Per-class imports: include nested classes so the body's references
    // resolve to `Outer$Inner` rather than collapsing to `Object`.
    let mut class_imports = imports.clone();
    let nesteds = nested_classes(c.body());
    for n in &nesteds {
        if let Some(ns) = n.name() {
            let nested_jvm = format!("{jvm_name_pre}${ns}");
            class_imports.entry(ns.to_string()).or_insert(nested_jvm);
        }
    }
    let imports = &class_imports;

    let (fields, ctor_params) = build_ctor_shape(c.primary_constructor(), imports, aliases);
    let (body_methods, body_props) = body_methods_and_props(c.body());
    let property_getters: Vec<ExternalMethod> = body_props
        .iter()
        .filter_map(|p| property_getter_method(*p, imports, aliases))
        .collect();
    let mut methods: Vec<ExternalMethod> = body_methods
        .iter()
        .map(|m| ext_method_from_fun(*m, imports, aliases))
        .collect();
    methods.extend(property_getters);
    // Synthesize getters for primary-ctor `val`/`var` params.
    // `class Circle(val radius: Double)` declares `radius` as both a
    // ctor param AND a property — kotlinc emits a private backing
    // field plus a public `getRadius()` (and `setRadius` for var).
    // Cross-file call sites need the getter signature to dispatch
    // `circle.radius` reads as `invokevirtual getRadius()D` instead
    // of falling back to an erased `()Object` shape.
    if let Some(pc) = c.primary_constructor() {
        if let Some(plist) = pc.value_parameter_list() {
            for p in plist.parameters() {
                if !p.is_val() && !p.is_var() {
                    continue;
                }
                let Some(pname) = p.name() else { continue };
                let mut chars = pname.chars();
                let Some(first_ch) = chars.next() else {
                    continue;
                };
                let getter_name = format!(
                    "get{}{}",
                    first_ch.to_uppercase().collect::<String>(),
                    chars.as_str()
                );
                let ret = p
                    .type_reference()
                    .map(|tr| type_ref_to_ty(tr, imports, aliases))
                    .unwrap_or(Ty::Any);
                methods.push(ExternalMethod {
                    name: getter_name,
                    params: Vec::new(),
                    return_ty: ret,
                    is_suspend: false,
                    is_inline: false,
                    is_abstract: false,
                    is_open: false,
                    receiver_ty: None,
                    annotations: Vec::new(),
                });
            }
        }
    }

    let (comp_methods, comp_props, has_companion) = companion_members(c.body());
    let comp_property_getters: Vec<ExternalMethod> = comp_props
        .iter()
        .filter_map(|p| property_getter_method(*p, imports, aliases))
        .collect();
    let mut companion_methods: Vec<ExternalMethod> = comp_methods
        .iter()
        .map(|m| ext_method_from_fun(*m, imports, aliases))
        .collect();
    companion_methods.extend(comp_property_getters);

    let secondary_ctors: Vec<ExternalConstructor> = c
        .body()
        .map(|b| {
            b.secondary_constructors()
                .map(|sc| ExternalConstructor {
                    params: secondary_ctor_params(sc, imports, aliases),
                })
                .collect()
        })
        .unwrap_or_default();

    let (super_class, iface_names) = collect_supertypes(c.super_type_list());
    let ext = ExternalClassDecl {
        jvm_name: jvm_name_pre.clone(),
        kind,
        fields,
        ctor_params,
        methods,
        secondary_ctors,
        companion_methods,
        has_companion,
        super_class,
        interfaces: iface_names,
        is_open: c.is_open(),
        is_abstract: c.is_abstract(),
        is_inner: c.is_inner(),
        enum_entries: Vec::new(),
        annotations: c.annotation_names().into_iter().map(String::from).collect(),
        has_type_params: c
            .type_parameter_list()
            .map(|tpl| tpl.parameters().next().is_some())
            .unwrap_or(false),
        has_init_blocks: c
            .body()
            .map(|b| b.anonymous_initializers().next().is_some())
            .unwrap_or(false),
    };
    register_class(table, &simple_name, ext);

    // Recurse into nested classes.
    let nested_outer = format!("{jvm_name_pre}$");
    for n in &nesteds {
        gather_class_recursive(*n, &nested_outer, table, imports, aliases);
    }
}

fn secondary_ctor_params(
    sc: KtSecondaryConstructor<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Vec<ExternalParam> {
    sc.value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| ext_param_from_value_parameter(p, imports, aliases))
                .collect()
        })
        .unwrap_or_default()
}

fn gather_object(
    o: KtObjectDeclaration<'_>,
    pkg_prefix: &str,
    table: &mut PackageSymbolTable,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    if o.visibility() == Visibility::Private {
        return;
    }
    let Some(name) = o.name() else { return };
    let jvm_name = format!("{pkg_prefix}{name}");
    let (body_methods, body_props) = body_methods_and_props(o.body());
    let property_getters: Vec<ExternalMethod> = body_props
        .iter()
        .filter_map(|p| property_getter_method(*p, imports, aliases))
        .collect();
    let mut methods: Vec<ExternalMethod> = body_methods
        .iter()
        .map(|m| ext_method_from_fun(*m, imports, aliases))
        .collect();
    methods.extend(property_getters);
    let (super_class, iface_names) = collect_supertypes(o.super_type_list());
    let ext = ExternalClassDecl {
        jvm_name: jvm_name.clone(),
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
        annotations: o
            .modifier_list()
            .map(|m| {
                m.annotations()
                    .filter_map(|a| a.short_name())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        has_type_params: false,
        has_init_blocks: o
            .body()
            .map(|b| b.anonymous_initializers().next().is_some())
            .unwrap_or(false),
    };
    register_class(table, name, ext);
}

fn gather_interface(
    i: KtInterface<'_>,
    pkg_prefix: &str,
    table: &mut PackageSymbolTable,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    if i.visibility() == Visibility::Private {
        return;
    }
    let Some(name) = i.name() else { return };
    let jvm_name = format!("{pkg_prefix}{name}");
    let (body_methods, body_props) = body_methods_and_props(i.body());
    let property_getters: Vec<ExternalMethod> = body_props
        .iter()
        .filter_map(|p| property_getter_method(*p, imports, aliases))
        .collect();
    let mut methods: Vec<ExternalMethod> = body_methods
        .iter()
        .map(|m| ext_method_from_fun(*m, imports, aliases))
        .collect();
    methods.extend(property_getters);
    let (super_class, iface_names) = collect_supertypes(i.super_type_list());
    let kind = if i.is_sealed() {
        ExternalClassKind::SealedInterface
    } else {
        ExternalClassKind::Interface
    };
    let ext = ExternalClassDecl {
        jvm_name: jvm_name.clone(),
        kind,
        fields: Vec::new(),
        ctor_params: Vec::new(),
        methods,
        secondary_ctors: Vec::new(),
        companion_methods: Vec::new(),
        has_companion: false,
        super_class,
        interfaces: iface_names,
        is_open: false,
        is_abstract: true,
        is_inner: false,
        enum_entries: Vec::new(),
        annotations: i
            .modifier_list()
            .map(|m| {
                m.annotations()
                    .filter_map(|a| a.short_name())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        has_type_params: i
            .type_parameter_list()
            .map(|tpl| tpl.parameters().next().is_some())
            .unwrap_or(false),
        has_init_blocks: false,
    };
    register_class(table, name, ext);
}

fn gather_enum(
    e: KtEnumClass<'_>,
    pkg_prefix: &str,
    table: &mut PackageSymbolTable,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    if e.visibility() == Visibility::Private {
        return;
    }
    let Some(name) = e.name() else { return };
    let jvm_name = format!("{pkg_prefix}{name}");
    let (fields, ctor_params) = build_ctor_shape(e.primary_constructor(), imports, aliases);
    let (body_methods, body_props) = body_methods_and_props(e.body());
    let property_getters: Vec<ExternalMethod> = body_props
        .iter()
        .filter_map(|p| property_getter_method(*p, imports, aliases))
        .collect();
    let mut methods: Vec<ExternalMethod> = body_methods
        .iter()
        .map(|m| ext_method_from_fun(*m, imports, aliases))
        .collect();
    methods.extend(property_getters);
    let enum_entries: Vec<String> = e
        .body()
        .map(|b| {
            b.enum_entries()
                .filter_map(|ee: KtEnumEntry<'_>| ee.name().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let (_super, iface_names) = collect_supertypes(e.super_type_list());
    let ext = ExternalClassDecl {
        jvm_name: jvm_name.clone(),
        kind: ExternalClassKind::Enum,
        fields,
        ctor_params,
        methods,
        secondary_ctors: Vec::new(),
        companion_methods: Vec::new(),
        has_companion: false,
        // Enums always extend `java/lang/Enum` at the JVM level. The
        // source-level name is `kotlin/Enum` but the compiler erases it
        // (see legacy gather_declarations enum arm).
        super_class: Some("java/lang/Enum".to_string()),
        interfaces: iface_names,
        is_open: false,
        is_abstract: false,
        is_inner: false,
        enum_entries,
        annotations: e
            .modifier_list()
            .map(|m| {
                m.annotations()
                    .filter_map(|a| a.short_name())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        has_type_params: false,
        has_init_blocks: e
            .body()
            .map(|b| b.anonymous_initializers().next().is_some())
            .unwrap_or(false),
    };
    register_class(table, name, ext);
}

fn register_class(table: &mut PackageSymbolTable, simple_name: &str, ext: ExternalClassDecl) {
    let fq = ext.jvm_name.clone();
    table
        .simple_name_to_fq
        .insert(simple_name.to_string(), fq.clone());
    table.classes_by_fq.insert(fq, ext.clone());
    table.classes.insert(simple_name.to_string(), ext);
}

// ── Top-level gather ────────────────────────────────────────────────

/// Gather top-level declarations across files into a
/// [`PackageSymbolTable`].
pub fn gather_declarations<'a>(
    files: &[(KtFile<'a>, &str)],
    _interner: &Interner,
) -> PackageSymbolTable {
    let mut table = PackageSymbolTable::default();

    // ── pre-pass: per-file imports + same-package decl map + typealiases
    let file_state: Vec<FileState<'_>> = files
        .iter()
        .map(|(file, wrapper_class)| build_file_state(*file, wrapper_class, files))
        .collect();

    for (state, (file, _)) in file_state.iter().zip(files.iter()) {
        let pkg_prefix = state.pkg_prefix.clone();
        let imports = &state.imports;
        let aliases = &state.aliases;
        let fq_wrapper = format!("{pkg_prefix}{}", state.wrapper_class);

        for decl in file.decls() {
            match decl {
                KtDecl::Fun(f) => {
                    if f.visibility() == Visibility::Private {
                        continue;
                    }
                    let Some(name) = f.name() else { continue };
                    let descriptor = build_method_descriptor(
                        f.value_parameter_list(),
                        f.return_type(),
                        f.receiver_type(),
                        imports,
                        aliases,
                    );
                    let param_tys: Vec<Ty> = f
                        .value_parameter_list()
                        .map(|pl| {
                            pl.parameters()
                                .map(|p| {
                                    p.type_reference()
                                        .map(|tr| type_ref_to_ty(tr, imports, aliases))
                                        .unwrap_or(Ty::Any)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let return_ty = f
                        .return_type()
                        .map(|tr| type_ref_to_ty(tr, imports, aliases))
                        .unwrap_or_else(|| infer_body_return_ty(f));
                    let has_default: Vec<bool> = f
                        .value_parameter_list()
                        .map(|pl| {
                            pl.parameters()
                                .map(|p| p.default_value().is_some())
                                .collect()
                        })
                        .unwrap_or_default();
                    let is_vararg: Vec<bool> = f
                        .value_parameter_list()
                        .map(|pl| pl.parameters().map(|p| p.is_vararg()).collect())
                        .unwrap_or_default();
                    let receiver_ty = f
                        .receiver_type()
                        .map(|tr| type_ref_to_ty(tr, imports, aliases));
                    let ext = ExternalFunDecl {
                        owner_class: fq_wrapper.clone(),
                        descriptor,
                        return_ty,
                        param_count: param_tys.len(),
                        param_tys,
                        is_suspend: f.is_suspend(),
                        is_inline: f.is_inline(),
                        is_extension: receiver_ty.is_some(),
                        receiver_ty,
                        has_default,
                        is_vararg,
                        annotations: f.annotation_names().into_iter().map(String::from).collect(),
                    };
                    table
                        .functions
                        .entry(name.to_string())
                        .or_default()
                        .push(ext);
                }
                KtDecl::Property(p) => {
                    if p.visibility() == Visibility::Private {
                        continue;
                    }
                    let Some(name) = p.name() else { continue };
                    let ty = p
                        .type_reference()
                        .map(|tr| type_ref_to_ty(tr, imports, aliases))
                        .unwrap_or(Ty::Any);
                    table.vals.insert(
                        name.to_string(),
                        ExternalValDecl {
                            owner_class: fq_wrapper.clone(),
                            ty,
                            annotations: p
                                .annotation_names()
                                .into_iter()
                                .map(String::from)
                                .collect(),
                        },
                    );
                }
                KtDecl::Class(c) => {
                    gather_class_recursive(c, &pkg_prefix, &mut table, imports, aliases);
                }
                KtDecl::Object(o) => {
                    gather_object(o, &pkg_prefix, &mut table, imports, aliases);
                }
                KtDecl::Interface(i) => {
                    gather_interface(i, &pkg_prefix, &mut table, imports, aliases);
                }
                KtDecl::EnumClass(e) => {
                    gather_enum(e, &pkg_prefix, &mut table, imports, aliases);
                }
                KtDecl::TypeAlias(_) => {
                    // The alias is registered into `aliases` during
                    // the pre-pass; no separate entry in the table.
                }
            }
        }
    }
    table
}

/// Per-file state — imports, typealiases, same-package decls.
struct FileState<'a> {
    pkg_prefix: String,
    wrapper_class: &'a str,
    imports: FxHashMap<String, String>,
    aliases: FxHashMap<String, AliasTarget>,
}

fn build_file_state<'a>(
    file: KtFile<'a>,
    wrapper_class: &'a str,
    all_files: &[(KtFile<'a>, &str)],
) -> FileState<'a> {
    let pkg_prefix = pkg_prefix_for(file);
    let mut imports: FxHashMap<String, String> = FxHashMap::default();
    let mut aliases: FxHashMap<String, AliasTarget> = FxHashMap::default();
    // Explicit imports.
    if let Some(import_list) = file.import_list() {
        for imp in skotch_ast::typed_children::<skotch_ast::KtImportDirective>(import_list.syntax())
        {
            if imp.is_wildcard() {
                continue;
            }
            let parts = imp.name_parts();
            if parts.is_empty() {
                continue;
            }
            let fq = parts.join("/");
            let simple = imp
                .alias()
                .and_then(|a| a.name())
                .unwrap_or_else(|| parts.last().copied().unwrap_or(""));
            if !simple.is_empty() {
                imports.insert(simple.to_string(), fq);
            }
        }
    }
    // Same-package decls + typealiases across all files.
    let this_pkg = pkg_path_for(file);
    for (other, _) in all_files {
        if pkg_path_for(*other) != this_pkg {
            continue;
        }
        for d in other.decls() {
            match d {
                KtDecl::Class(c) => {
                    if let Some(n) = c.name() {
                        imports
                            .entry(n.to_string())
                            .or_insert_with(|| format!("{pkg_prefix}{n}"));
                    }
                }
                KtDecl::Object(o) => {
                    if let Some(n) = o.name() {
                        imports
                            .entry(n.to_string())
                            .or_insert_with(|| format!("{pkg_prefix}{n}"));
                    }
                }
                KtDecl::Interface(i) => {
                    if let Some(n) = i.name() {
                        imports
                            .entry(n.to_string())
                            .or_insert_with(|| format!("{pkg_prefix}{n}"));
                    }
                }
                KtDecl::EnumClass(e) => {
                    if let Some(n) = e.name() {
                        imports
                            .entry(n.to_string())
                            .or_insert_with(|| format!("{pkg_prefix}{n}"));
                    }
                }
                KtDecl::TypeAlias(t) => {
                    if let (Some(name), Some(target)) = (t.name(), t.type_reference()) {
                        aliases.insert(name.to_string(), AliasTarget::from_type_ref(target));
                    }
                }
                _ => {}
            }
        }
    }
    FileState {
        pkg_prefix,
        wrapper_class,
        imports,
        aliases,
    }
}

fn pkg_prefix_for(file: KtFile<'_>) -> String {
    file.package_directive()
        .map(|p| {
            let name = p.name();
            if name.is_empty() {
                String::new()
            } else {
                format!("{}/", name.replace('.', "/"))
            }
        })
        .unwrap_or_default()
}

fn pkg_path_for(file: KtFile<'_>) -> String {
    file.package_directive()
        .map(|p| p.name())
        .unwrap_or_default()
}

// ── resolve_file ────────────────────────────────────────────────────

/// Resolve identifier references in a single file. Same shape as
/// [`crate::resolve_file`] but consumes the typed AST.
pub fn resolve_file(
    file: KtFile<'_>,
    interner: &mut Interner,
    package_symbols: Option<&PackageSymbolTable>,
) -> ResolvedFile {
    let mut out = ResolvedFile::default();

    // ── Built-in / stdlib intrinsics ────────────────────────────────
    let println_sym = interner.intern("println");
    out.top_level.insert(println_sym, DefId::PrintlnIntrinsic);
    let print_sym = interner.intern("print");
    out.top_level.insert(print_sym, DefId::PrintlnIntrinsic);
    for name in skotch_types::intrinsics::STDLIB_TOP_LEVEL_NAMES {
        let sym = interner.intern(name);
        out.top_level.insert(sym, DefId::PrintlnIntrinsic);
    }

    // ── Pass 1: register every top-level decl with a DefId ─────────
    let mut fn_idx = 0u32;
    let mut val_idx = 0u32;
    for decl in file.decls() {
        match decl {
            KtDecl::Fun(f) => {
                if let Some(name) = f.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::Function(fn_idx));
                }
                fn_idx += 1;
            }
            KtDecl::Property(p) => {
                if let Some(name) = p.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::TopLevelVal(val_idx));
                }
                val_idx += 1;
            }
            KtDecl::Class(c) => {
                if let Some(name) = c.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::Object(o) => {
                if let Some(name) = o.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::EnumClass(e) => {
                if let Some(name) = e.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::Interface(i) => {
                if let Some(name) = i.name() {
                    let sym = interner.intern(name);
                    out.top_level.insert(sym, DefId::PossibleExternal(sym));
                }
            }
            KtDecl::TypeAlias(_) => {}
        }
    }

    // ── Cross-file ExternalPackage entries (local wins) ────────────
    if let Some(pkg) = package_symbols {
        for name in pkg.functions.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.vals.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
        for name in pkg.classes.keys() {
            let sym = interner.intern(name);
            out.top_level
                .entry(sym)
                .or_insert(DefId::ExternalPackage(sym));
        }
    }

    // ── Pass 2: per-function ResolvedFunction with body-walk ────────
    let mut fn_idx_for_body = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let rf = resolve_function_body(f, fn_idx_for_body, interner, &out.top_level);
            out.functions.push(rf);
            fn_idx_for_body += 1;
        }
    }

    // ── Per-top-val ResolvedTopLevelVal with initializer ref walk ──
    for decl in file.decls() {
        if let KtDecl::Property(p) = decl {
            let Some(name) = p.name() else { continue };
            let name_sym = interner.intern(name);
            let mut init_refs: Vec<crate::ResolvedRef> = Vec::new();
            if let Some(init) = p.initializer() {
                let scope: Vec<(Symbol, DefId)> = Vec::new();
                let mut tmp_rf = ResolvedFunction {
                    name: name_sym,
                    params: Vec::new(),
                    locals: Vec::new(),
                    body_refs: Vec::new(),
                };
                resolve_expr(
                    &init,
                    u32::MAX,
                    &mut scope.clone(),
                    &mut tmp_rf,
                    interner,
                    &out.top_level,
                );
                init_refs = tmp_rf.body_refs;
            }
            out.top_vals.push(crate::ResolvedTopLevelVal {
                name: name_sym,
                init_refs,
            });
        }
    }

    out
}

// ── Body-walk Resolver ──────────────────────────────────────────────
//
// Mirrors the legacy `Resolver` impl. Walks each function body to
// collect `ResolvedRef` entries (one per identifier reference) and
// the locals table. Scopes are stack-allocated; `is`/`as` smart-cast
// scopes are still TODO.

fn resolve_function_body(
    f: KtFun<'_>,
    fn_idx: u32,
    interner: &mut Interner,
    top_level: &rustc_hash::FxHashMap<Symbol, DefId>,
) -> ResolvedFunction {
    use skotch_syntax::SyntaxKind as S;
    let name_sym = f
        .name()
        .map(|n| interner.intern(n))
        .unwrap_or_else(|| interner.intern("<anonymous>"));

    let mut scope: Vec<(Symbol, DefId)> = Vec::new();

    // Extension fn → `this` is param 0.
    let has_receiver = f.receiver_type().is_some();
    if has_receiver {
        let this_sym = interner.intern("this");
        scope.push((this_sym, DefId::Param(fn_idx, 0)));
    }
    let param_offset = if has_receiver { 1u32 } else { 0 };
    let params_vec: Vec<Symbol> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .filter_map(|p| p.name().map(|n| interner.intern(n)))
                .collect()
        })
        .unwrap_or_default();
    for (i, sym) in params_vec.iter().enumerate() {
        scope.push((*sym, DefId::Param(fn_idx, i as u32 + param_offset)));
    }

    let mut rf = ResolvedFunction {
        name: name_sym,
        params: params_vec,
        locals: Vec::new(),
        body_refs: Vec::new(),
    };

    if let Some(block) = f.body_block() {
        resolve_block(block, fn_idx, &mut scope, &mut rf, interner, top_level);
    } else if let Some(e) = f.body_expression() {
        resolve_expr(&e, fn_idx, &mut scope, &mut rf, interner, top_level);
    }

    let _ = S::EOF; // Silence unused-import on the rare branch.
    rf
}

fn resolve_block(
    block: skotch_ast::KtBlock<'_>,
    fn_idx: u32,
    scope: &mut Vec<(Symbol, DefId)>,
    rf: &mut ResolvedFunction,
    interner: &mut Interner,
    top_level: &rustc_hash::FxHashMap<Symbol, DefId>,
) {
    use skotch_ast::KtExpr;
    let saved = scope.len();
    // Walk children directly so we can match either PROPERTY (local
    // val/var) or KtExpr. block.statements() only yields KtExpr.
    for c in skotch_ast::children(block.syntax()) {
        if let Some(prop) = skotch_ast::KtProperty::cast(c) {
            // First resolve the initializer (referring to symbols in
            // current scope before this local enters scope), then
            // register the local under its name.
            if let Some(init) = prop.initializer() {
                resolve_expr(&init, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(name) = prop.name() {
                let sym = interner.intern(name);
                let local_idx = rf.locals.len() as u32;
                rf.locals.push(sym);
                scope.push((sym, DefId::Local(fn_idx, local_idx)));
            }
            continue;
        }
        if let Some(e) = KtExpr::cast(c) {
            resolve_expr(&e, fn_idx, scope, rf, interner, top_level);
        }
    }
    scope.truncate(saved);
}

fn resolve_expr(
    e: &skotch_ast::KtExpr<'_>,
    fn_idx: u32,
    scope: &mut Vec<(Symbol, DefId)>,
    rf: &mut ResolvedFunction,
    interner: &mut Interner,
    top_level: &rustc_hash::FxHashMap<Symbol, DefId>,
) {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Reference(r) => {
            if let Some(name) = r.name() {
                let sym = interner.intern(name);
                let def = scope
                    .iter()
                    .rev()
                    .find_map(|(s, d)| if *s == sym { Some(*d) } else { None })
                    .or_else(|| top_level.get(&sym).copied())
                    .unwrap_or(DefId::PossibleExternal(sym));
                rf.body_refs.push(crate::ResolvedRef {
                    span: r.span(),
                    def,
                });
            }
        }
        KtExpr::This(t) => {
            // `this` keyword — resolves to the function's receiver
            // (extension fn) or the enclosing class's instance.
            let this_sym = interner.intern("this");
            let def = scope
                .iter()
                .rev()
                .find_map(|(s, d)| if *s == this_sym { Some(*d) } else { None })
                .unwrap_or(DefId::PossibleExternal(this_sym));
            rf.body_refs.push(crate::ResolvedRef {
                span: t.span(),
                def,
            });
        }
        KtExpr::Super(s) => {
            let super_sym = interner.intern("super");
            rf.body_refs.push(crate::ResolvedRef {
                span: s.span(),
                def: DefId::PossibleExternal(super_sym),
            });
        }
        KtExpr::Call(c) => {
            if let Some(callee) = c.callee() {
                resolve_expr(&callee, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(args) = c.value_argument_list() {
                for a in args.arguments() {
                    if let Some(av) = a.expression() {
                        resolve_expr(&av, fn_idx, scope, rf, interner, top_level);
                    }
                }
            }
        }
        KtExpr::Binary(b) => {
            if let Some(l) = b.lhs() {
                resolve_expr(&l, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(r) = b.rhs() {
                resolve_expr(&r, fn_idx, scope, rf, interner, top_level);
            }
        }
        KtExpr::DotQualified(d) => {
            // The receiver of `a.b.c`. We resolve `a`; `b` and `c` are
            // member-name idents whose resolution requires type
            // information (handled in mir-lower).
            for c in skotch_ast::children(d.syntax()) {
                if let Some(child_e) = KtExpr::cast(c) {
                    resolve_expr(&child_e, fn_idx, scope, rf, interner, top_level);
                    break; // Only resolve the leftmost (receiver).
                }
            }
        }
        KtExpr::SafeAccess(s) => {
            for c in skotch_ast::children(s.syntax()) {
                if let Some(child_e) = KtExpr::cast(c) {
                    resolve_expr(&child_e, fn_idx, scope, rf, interner, top_level);
                    break;
                }
            }
        }
        KtExpr::If(i) => {
            if let Some(cond) = i.condition().and_then(|c| c.expression()) {
                resolve_expr(&cond, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(t) = i.then_branch().and_then(|t| t.expression()) {
                resolve_expr(&t, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(el) = i.else_branch().and_then(|e| e.expression()) {
                resolve_expr(&el, fn_idx, scope, rf, interner, top_level);
            }
        }
        KtExpr::While(w) => {
            if let Some(c) = w.condition().and_then(|c| c.expression()) {
                resolve_expr(&c, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(b) = w.body().and_then(|b| b.expression()) {
                resolve_expr(&b, fn_idx, scope, rf, interner, top_level);
            }
        }
        KtExpr::DoWhile(w) => {
            if let Some(b) = w.body().and_then(|b| b.expression()) {
                resolve_expr(&b, fn_idx, scope, rf, interner, top_level);
            }
            if let Some(c) = w.condition().and_then(|c| c.expression()) {
                resolve_expr(&c, fn_idx, scope, rf, interner, top_level);
            }
        }
        KtExpr::For(fo) => {
            if let Some(range) = fo.loop_range().and_then(|r| r.expression()) {
                resolve_expr(&range, fn_idx, scope, rf, interner, top_level);
            }
            // Push the loop parameter into scope, then walk body.
            let saved = scope.len();
            if let Some(p) = fo.loop_parameter() {
                if let Some(name) = p.name() {
                    let sym = interner.intern(name);
                    let local_idx = rf.locals.len() as u32;
                    rf.locals.push(sym);
                    scope.push((sym, DefId::Local(fn_idx, local_idx)));
                }
            }
            if let Some(b) = fo.body().and_then(|b| b.expression()) {
                resolve_expr(&b, fn_idx, scope, rf, interner, top_level);
            }
            scope.truncate(saved);
        }
        KtExpr::When(w) => {
            if let Some(subject) = w.subject() {
                resolve_expr(&subject, fn_idx, scope, rf, interner, top_level);
            }
            for entry in w.entries() {
                // Each arm's body resolves in its own scope; smart-cast
                // narrowing not yet propagated here.
                let saved = scope.len();
                if let Some(b) = entry.body() {
                    resolve_expr(&b, fn_idx, scope, rf, interner, top_level);
                }
                scope.truncate(saved);
            }
        }
        KtExpr::Try(t) => {
            if let Some(b) = t.try_block() {
                resolve_block(b, fn_idx, scope, rf, interner, top_level);
            }
            for catch in t.catches() {
                let saved = scope.len();
                if let Some(p) = catch.parameter() {
                    if let Some(name) = p.name() {
                        let sym = interner.intern(name);
                        let local_idx = rf.locals.len() as u32;
                        rf.locals.push(sym);
                        scope.push((sym, DefId::Local(fn_idx, local_idx)));
                    }
                }
                if let Some(b) = catch.body() {
                    resolve_block(b, fn_idx, scope, rf, interner, top_level);
                }
                scope.truncate(saved);
            }
            if let Some(fin) = t.finally() {
                if let Some(b) = fin.body() {
                    resolve_block(b, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        KtExpr::Lambda(l) => {
            // Lambda introduces a new scope; capture analysis is
            // upstream of typeck — here we just walk the body to
            // resolve references against the outer scope so undeclared
            // identifiers still go through PossibleExternal fallback.
            let saved = scope.len();
            if let Some(fl) = l.function_literal() {
                if let Some(plist) = fl.value_parameter_list() {
                    for p in plist.parameters() {
                        if let Some(name) = p.name() {
                            let sym = interner.intern(name);
                            let local_idx = rf.locals.len() as u32;
                            rf.locals.push(sym);
                            scope.push((sym, DefId::Local(fn_idx, local_idx)));
                        }
                    }
                }
                if let Some(b) = fl.body() {
                    resolve_block(b, fn_idx, scope, rf, interner, top_level);
                }
            }
            scope.truncate(saved);
        }
        KtExpr::Return(r) => {
            for c in skotch_ast::children(r.syntax()) {
                if let Some(child) = KtExpr::cast(c) {
                    resolve_expr(&child, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        KtExpr::Throw(t) => {
            for c in skotch_ast::children(t.syntax()) {
                if let Some(child) = KtExpr::cast(c) {
                    resolve_expr(&child, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        KtExpr::Block(b) => {
            resolve_block(*b, fn_idx, scope, rf, interner, top_level);
        }
        KtExpr::Parenthesized(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(child) = KtExpr::cast(c) {
                    resolve_expr(&child, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        KtExpr::Prefix(_) | KtExpr::Postfix(_) | KtExpr::Unary(_) => {
            // Skip operator-only; the operand resolves via further KtExpr cast.
            for c in skotch_ast::children(e.syntax()) {
                if let Some(child) = KtExpr::cast(c) {
                    resolve_expr(&child, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        KtExpr::String(t) => {
            // Walk short-template-entry / block-template-entry children.
            for c in skotch_ast::children(t.syntax()) {
                if let Some(child) = KtExpr::cast(c) {
                    resolve_expr(&child, fn_idx, scope, rf, interner, top_level);
                }
            }
        }
        // Leaf constants / others: no further work for now.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> skotch_ast::ParsedFile {
        skotch_ast::parse("test.kt", src)
    }

    #[test]
    fn gather_top_level_fun_with_descriptor() {
        let p = parse("fun add(a: Int, b: Int): Int = a + b");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let f = &table.functions["add"][0];
        assert_eq!(f.descriptor, "(II)I");
        assert_eq!(f.param_count, 2);
        assert_eq!(f.return_ty, Ty::Int);
        assert_eq!(f.owner_class, "TestKt");
    }

    #[test]
    fn gather_class_with_primary_ctor() {
        let p = parse("class P(val x: Int, val y: Int)");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("P").expect("class P");
        assert_eq!(c.jvm_name, "P");
        assert_eq!(c.fields.len(), 2);
        assert_eq!(c.fields[0], ("x".to_string(), Ty::Int));
    }

    #[test]
    fn gather_data_class_kind() {
        let p = parse("data class P(val x: Int)");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("P").expect("class P");
        assert_eq!(c.kind, ExternalClassKind::DataClass);
    }

    #[test]
    fn gather_skips_private_decls() {
        let p = parse("private fun hidden() {}\nfun visible() {}");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        assert!(!table.functions.contains_key("hidden"));
        assert!(table.functions.contains_key("visible"));
    }

    #[test]
    fn gather_interface() {
        let p = parse("interface Printable { fun pretty(): String }");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("Printable").expect("Printable iface");
        assert_eq!(c.kind, ExternalClassKind::Interface);
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name, "pretty");
        assert_eq!(c.methods[0].return_ty, Ty::String);
    }

    #[test]
    fn gather_enum_entries() {
        let p = parse("enum class Color { RED, GREEN, BLUE }");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("Color").expect("enum");
        assert_eq!(c.kind, ExternalClassKind::Enum);
        assert_eq!(c.enum_entries, vec!["RED", "GREEN", "BLUE"]);
    }

    #[test]
    fn gather_object_singleton() {
        let p = parse("object Singleton { fun greet(): String = \"hi\" }");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("Singleton").expect("object");
        assert_eq!(c.kind, ExternalClassKind::Object);
    }

    #[test]
    fn gather_extension_function_receiver() {
        let p = parse("fun String.exclaim(): String = this + \"!\"");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let f = &table.functions["exclaim"][0];
        assert!(f.is_extension);
        assert_eq!(f.receiver_ty, Some(Ty::String));
    }

    #[test]
    fn resolve_assigns_def_ids() {
        let p = parse("fun a() {}\nfun b() {}");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let a = interner.intern("a");
        let b = interner.intern("b");
        assert_eq!(r.top_level.get(&a), Some(&DefId::Function(0)));
        assert_eq!(r.top_level.get(&b), Some(&DefId::Function(1)));
    }

    #[test]
    fn resolve_collects_params() {
        let p = parse("fun add(a: Int, b: Int): Int = a + b");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        assert_eq!(r.functions.len(), 1);
        let f = &r.functions[0];
        assert_eq!(f.params.len(), 2);
        assert_eq!(interner.resolve(f.params[0]), "a");
        assert_eq!(interner.resolve(f.params[1]), "b");
    }

    #[test]
    fn package_prefix_applied_to_jvm_name() {
        let p = parse("package com.foo\nclass Bar");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let c = table.classes.get("Bar").expect("class");
        assert_eq!(c.jvm_name, "com/foo/Bar");
    }

    #[test]
    fn typealias_substitution_in_descriptor() {
        let p = parse(
            "typealias Predicate = (Int) -> Boolean\nfun apply(p: Predicate): Boolean = true",
        );
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let f = &table.functions["apply"][0];
        assert_eq!(f.descriptor, "(Lkotlin/jvm/functions/Function1;)Z");
    }

    #[test]
    fn nullable_type_in_descriptor() {
        let p = parse("fun maybe(x: String?): String? = null");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let f = &table.functions["maybe"][0];
        assert_eq!(f.descriptor, "(Ljava/lang/Object;)Ljava/lang/Object;");
    }

    #[test]
    fn nested_classes_inner_jvm_name() {
        let p = parse("class Outer { class Inner }");
        let interner = Interner::new();
        let table = gather_declarations(&[(p.file(), "TestKt")], &interner);
        let outer = table.classes.get("Outer").expect("outer");
        assert_eq!(outer.jvm_name, "Outer");
        let inner = table
            .classes_by_fq
            .get("Outer$Inner")
            .expect("inner via FQ");
        assert_eq!(inner.jvm_name, "Outer$Inner");
    }

    #[test]
    fn resolve_body_tracks_param_reference() {
        let p = parse("fun add(a: Int, b: Int): Int = a + b");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        // The body walks `a + b`. Each ident becomes a ResolvedRef
        // pointing to Param(0, 0) or Param(0, 1).
        let a = interner.intern("a");
        let b = interner.intern("b");
        let mut saw_a = false;
        let mut saw_b = false;
        for ref_ in &f.body_refs {
            match ref_.def {
                DefId::Param(0, 0) => saw_a = true,
                DefId::Param(0, 1) => saw_b = true,
                _ => {}
            }
        }
        assert!(
            saw_a,
            "expected ref to a as Param(0,0); refs={:?}",
            f.body_refs
        );
        assert!(saw_b, "expected ref to b as Param(0,1)");
        // Symbol IDs round-trip through the interner.
        let _ = (a, b);
    }

    #[test]
    fn resolve_body_tracks_top_level_function_call() {
        let p = parse("fun helper(): Int = 1\nfun main() { helper() }");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let main_fn = &r.functions[1];
        let helper_def = main_fn
            .body_refs
            .iter()
            .find(|rf| matches!(rf.def, DefId::Function(_)))
            .expect("expected ref to helper");
        assert_eq!(helper_def.def, DefId::Function(0));
    }

    #[test]
    fn resolve_body_tracks_local_val_declaration() {
        let p = parse("fun main() {\n  val x = 1\n  println(x)\n}");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        // The local val `x` should be in f.locals.
        let x_sym = interner.intern("x");
        assert!(
            f.locals.contains(&x_sym),
            "expected local x; locals={:?}",
            f.locals
        );
        // The reference `x` inside println should resolve to Local(0,0).
        let x_ref = f
            .body_refs
            .iter()
            .find(|rf| matches!(rf.def, DefId::Local(0, 0)))
            .expect("expected ref to local x as Local(0,0)");
        assert_eq!(x_ref.def, DefId::Local(0, 0));
    }

    #[test]
    fn body_walk_for_loop_introduces_local() {
        let p = parse("fun main() { for (i in 0..10) { println(i) } }");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        let i_sym = interner.intern("i");
        assert!(
            f.locals.contains(&i_sym),
            "expected loop var i; locals={:?}",
            f.locals
        );
    }

    #[test]
    fn body_walk_while_loop_resolves_condition() {
        let p = parse("fun main() {\n  val x = 1\n  while (x > 0) { println(x) }\n}");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        // Multiple references to x (in condition + body); they should all resolve to Local(0,0).
        let local_refs = f
            .body_refs
            .iter()
            .filter(|rf| matches!(rf.def, DefId::Local(0, 0)))
            .count();
        assert!(
            local_refs >= 2,
            "expected ≥2 refs to local x; got {local_refs}"
        );
    }

    #[test]
    fn body_walk_when_arm() {
        let p = parse(
            "fun main() {\n  val x = 1\n  when (x) {\n    1 -> println(\"one\")\n    else -> println(\"other\")\n  }\n}",
        );
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        // Subject `x` should resolve to Local(0,0).
        assert!(
            f.body_refs
                .iter()
                .any(|rf| matches!(rf.def, DefId::Local(0, 0))),
            "expected ref to local x as subject; refs={:?}",
            f.body_refs
        );
    }

    #[test]
    fn body_walk_try_catch_introduces_exception_var() {
        let p =
            parse("fun main() {\n  try { println(\"hi\") } catch (e: Exception) { println(e) }\n}");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        let e_sym = interner.intern("e");
        assert!(
            f.locals.contains(&e_sym),
            "expected exception var e; locals={:?}",
            f.locals
        );
    }

    #[test]
    fn extension_function_this_param() {
        let p = parse("fun String.exclaim(): String = this");
        let mut interner = Interner::new();
        let r = resolve_file(p.file(), &mut interner, None);
        let f = &r.functions[0];
        // The body just references `this` — Param(0, 0) (receiver).
        let this_ref = f
            .body_refs
            .iter()
            .find(|rf| matches!(rf.def, DefId::Param(0, 0)))
            .expect("expected ref to this");
        assert_eq!(this_ref.def, DefId::Param(0, 0));
    }

    #[test]
    fn cross_file_same_package_class_in_import_map() {
        let a = parse("package com.x\nclass A");
        let b = parse("package com.x\nclass UsesA { fun f(): A = A() }");
        let interner = Interner::new();
        let table = gather_declarations(&[(a.file(), "AKt"), (b.file(), "BKt")], &interner);
        // A should be imported into B's lookup map so its method
        // descriptor uses Lcom/x/A; not Ljava/lang/Object;.
        let uses_a = table.classes_by_fq.get("com/x/UsesA").expect("UsesA");
        let m = uses_a.methods.iter().find(|m| m.name == "f").expect("f");
        // return type via type_ref_to_ty -> Ty::Class(com/x/A)
        match &m.return_ty {
            Ty::Class(n) => assert_eq!(n, "com/x/A"),
            other => panic!("expected Class(com/x/A), got {other:?}"),
        }
    }
}
