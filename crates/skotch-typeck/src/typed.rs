//! Typed-AST entry point for type checking.
//!
//! Parallel to the legacy [`crate::type_check`] but takes a
//! [`skotch_ast::KtFile`] (typed view over a SIL tree) instead of the
//! Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Pass 1 (signature collection):
//! - Top-level fun: param/return Ty from KtTypeReference, with
//!   typealias substitution and import resolution.
//! - Top-level val: declared Ty (or `Ty::Any` when omitted).
//! - Class / interface / enum / object: members threaded into the
//!   per-file `TypedFile` so cross-file consumers can read them.
//!
//! Not yet covered (next migration sessions):
//! - Function body type inference (the legacy bidirectional checker).
//! - `when` exhaustiveness over enum / sealed subjects.
//! - Smart-cast narrowing on `is`/`as` / `requireNotNull`.
//! - Cycle detection across top-level vals.

use crate::{Signature, TypedFile, TypedFunction, TypedTopVal};
use rustc_hash::FxHashMap;
use skotch_ast::{
    AstNode, KtClass, KtClassBody, KtDecl, KtEnumClass, KtFile, KtFun, KtFunctionType, KtInterface,
    KtObjectDeclaration, KtTypeReference, KtUserType, KtValueParameter, KtValueParameterList,
};
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_resolve::{DefId, PackageSymbolTable, ResolvedFile};
use skotch_types::Ty;

// ── TypeEnv: per-file class / interface / enum / object signatures ──
//
// Mirrors the legacy `TypeEnv` in skotch-typeck/src/lib.rs but built
// from the typed AST. `synth_expr` consults it to resolve member
// calls (`receiver.method()`) and field access (`receiver.field`).

#[derive(Clone, Debug, Default)]
pub(crate) struct TypeEnv {
    /// Simple class name → declared shape.
    pub(crate) types: FxHashMap<String, TypeDecl>,
    /// Enum entry name → owning enum class name (e.g. `RED` → `Color`).
    pub(crate) enum_entries: FxHashMap<String, String>,
    /// Sealed class name → direct subclass names.
    pub(crate) sealed_subclasses: FxHashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // `name` reserved for future diagnostics
pub(crate) struct TypeDecl {
    pub(crate) name: String,
    pub(crate) super_class: Option<String>,
    pub(crate) interfaces: Vec<String>,
    pub(crate) fields: Vec<FieldSig>,
    pub(crate) methods: Vec<MethodSig>,
    pub(crate) companion_methods: Vec<MethodSig>,
    pub(crate) is_enum: bool,
    pub(crate) enum_entry_names: Vec<String>,
    pub(crate) is_sealed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FieldSig {
    pub(crate) name: String,
    pub(crate) ty: Ty,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // `params` reserved for future overload resolution
pub(crate) struct MethodSig {
    pub(crate) name: String,
    pub(crate) params: Vec<Ty>,
    pub(crate) ret: Ty,
}

impl TypeEnv {
    /// Walk a type's method table — including superclass and
    /// implemented interfaces — for the given method name.
    pub(crate) fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<&MethodSig> {
        let mut visited = rustc_hash::FxHashSet::default();
        let mut stack = vec![type_name.to_string()];
        while let Some(name) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            if let Some(d) = self.types.get(&name) {
                if let Some(m) = d.methods.iter().find(|m| m.name == method_name) {
                    return Some(m);
                }
                if let Some(sc) = d.super_class.as_ref() {
                    stack.push(sc.clone());
                }
                for i in &d.interfaces {
                    stack.push(i.clone());
                }
            }
        }
        None
    }

    pub(crate) fn lookup_field(&self, type_name: &str, field_name: &str) -> Option<&FieldSig> {
        let mut visited = rustc_hash::FxHashSet::default();
        let mut stack = vec![type_name.to_string()];
        while let Some(name) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            if let Some(d) = self.types.get(&name) {
                if let Some(f) = d.fields.iter().find(|f| f.name == field_name) {
                    return Some(f);
                }
                if let Some(sc) = d.super_class.as_ref() {
                    stack.push(sc.clone());
                }
            }
        }
        None
    }

    pub(crate) fn lookup_companion(
        &self,
        type_name: &str,
        method_name: &str,
    ) -> Option<&MethodSig> {
        self.types
            .get(type_name)
            .and_then(|d| d.companion_methods.iter().find(|m| m.name == method_name))
    }
}

/// Type-check a single file using the typed AST input.
pub fn type_check(
    file: KtFile<'_>,
    _resolved: &ResolvedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    package_symbols: Option<&PackageSymbolTable>,
) -> TypedFile {
    let mut out = TypedFile::default();

    // ── Imports / typealiases (per-file) ────────────────────────────
    let mut imports = collect_imports(file);
    // Add same-file class/interface/object/enum names so a body
    // reference to one of them maps to `Ty::Class(SimpleName)`.
    for d in file.decls() {
        match d {
            KtDecl::Class(c) => {
                if let Some(n) = c.name() {
                    imports
                        .entry(n.to_string())
                        .or_insert_with(|| n.to_string());
                }
            }
            KtDecl::Interface(i) => {
                if let Some(n) = i.name() {
                    imports
                        .entry(n.to_string())
                        .or_insert_with(|| n.to_string());
                }
            }
            KtDecl::Object(o) => {
                if let Some(n) = o.name() {
                    imports
                        .entry(n.to_string())
                        .or_insert_with(|| n.to_string());
                }
            }
            KtDecl::EnumClass(e) => {
                if let Some(n) = e.name() {
                    imports
                        .entry(n.to_string())
                        .or_insert_with(|| n.to_string());
                }
            }
            _ => {}
        }
    }
    let mut aliases: FxHashMap<String, AliasTarget> = FxHashMap::default();
    for d in file.decls() {
        if let KtDecl::TypeAlias(t) = d {
            if let (Some(name), Some(tr)) = (t.name(), t.type_reference()) {
                aliases.insert(name.to_string(), AliasTarget::from_type_ref(tr));
            }
        }
    }
    let _ = package_symbols; // future: thread cross-file aliases

    // ── TypeEnv: walk class/interface/enum/object decls ─────────────
    let env = build_type_env(file, &imports, &aliases);

    // ── Top-val cycle detection ─────────────────────────────────────
    let mut diags_ = Diagnostics::new();
    detect_top_val_cycles(file, &mut diags_);
    let _ = diags_; // diags integration pending

    // ── Pass 1: top-level signatures ────────────────────────────────
    let mut fn_index = 0u32;
    let mut val_index = 0u32;
    for decl in file.decls() {
        match decl {
            KtDecl::Fun(f) => {
                let param_tys = collect_param_tys(f.value_parameter_list(), &imports, &aliases);
                let return_ty = f
                    .return_type()
                    .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                    .unwrap_or_else(|| infer_return_ty(f));
                let sig = Signature {
                    params: param_tys.clone(),
                    ret: return_ty.clone(),
                };
                out.top_signatures.insert(DefId::Function(fn_index), sig);
                out.functions.push(TypedFunction {
                    name_index: fn_index,
                    return_ty,
                    param_tys,
                    local_tys: Vec::new(),
                });
                fn_index += 1;
            }
            KtDecl::Property(p) => {
                let ty = p
                    .type_reference()
                    .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                    .unwrap_or(Ty::Any);
                out.top_signatures.insert(
                    DefId::TopLevelVal(val_index),
                    Signature {
                        params: Vec::new(),
                        ret: ty.clone(),
                    },
                );
                out.top_vals.push(TypedTopVal {
                    name_index: val_index,
                    ty,
                });
                val_index += 1;
            }
            _ => {}
        }
    }

    // ── Pass 2: per-function body inference (basic) ─────────────────
    //
    // For each top-level fun we walk the body, recording locals types
    // in `TypedFunction.local_tys`. This is a minimal bidirectional
    // checker — literal and var-init shapes only. Full coverage
    // (operator overloading, smart casts, sealed exhaustiveness)
    // lands in subsequent sessions.
    //
    // Build a name->Ty map of top-level fun return types so calls
    // resolve through synth_expr at least at the simplest level.
    let mut fn_returns: FxHashMap<String, Ty> = FxHashMap::default();
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            if let Some(name) = f.name() {
                let ret = f
                    .return_type()
                    .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                    .unwrap_or_else(|| infer_return_ty(f));
                fn_returns.insert(name.to_string(), ret);
            }
        }
    }

    let mut fn_idx = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let mut local_tys: Vec<Ty> = Vec::new();
            // Seed the scope with this fn's parameters so an init
            // expression that references a param resolves to its type.
            let mut scope: Vec<(String, Ty)> = Vec::new();
            if let Some(plist) = f.value_parameter_list() {
                for p in plist.parameters() {
                    if let Some(name) = p.name() {
                        let ty = p
                            .type_reference()
                            .map(|tr| type_ref_to_ty(tr, &imports, &aliases))
                            .unwrap_or(Ty::Any);
                        scope.push((name.to_string(), ty));
                    }
                }
            }
            if let Some(block) = f.body_block() {
                walk_block_with_scope_and_fns(
                    block,
                    &imports,
                    &aliases,
                    &fn_returns,
                    &env,
                    &mut local_tys,
                    &mut scope,
                );
            }
            if let Some(rec) = out.functions.iter_mut().find(|r| r.name_index == fn_idx) {
                rec.local_tys = local_tys;
            }
            fn_idx += 1;
        }
    }

    out
}

// ── TypeEnv builder ─────────────────────────────────────────────────

fn build_type_env(
    file: KtFile<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> TypeEnv {
    let mut env = TypeEnv::default();
    for d in file.decls() {
        match d {
            KtDecl::Class(c) => register_class(c, &mut env, imports, aliases),
            KtDecl::Interface(i) => register_interface(i, &mut env, imports, aliases),
            KtDecl::Object(o) => register_object(o, &mut env, imports, aliases),
            KtDecl::EnumClass(e) => register_enum(e, &mut env, imports, aliases),
            _ => {}
        }
    }
    env
}

fn class_body_methods_props<'a>(
    body: Option<KtClassBody<'a>>,
) -> (Vec<skotch_ast::KtFun<'a>>, Vec<skotch_ast::KtProperty<'a>>) {
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

fn collect_super_class_iface(
    super_list: Option<skotch_ast::KtSuperTypeList<'_>>,
) -> (Option<String>, Vec<String>) {
    let Some(list) = super_list else {
        return (None, Vec::new());
    };
    let mut super_class = None;
    let mut ifaces = Vec::new();
    for entry in list.entries() {
        let name = entry
            .type_reference()
            .and_then(|t| t.user_type())
            .and_then(|u| u.name())
            .map(|s| s.to_string());
        let is_call = matches!(entry, skotch_ast::SuperTypeEntry::Call(_));
        match name {
            Some(n) if is_call => super_class = Some(n),
            Some(n) => ifaces.push(n),
            None => {}
        }
    }
    (super_class, ifaces)
}

fn method_sig_from_fun(
    f: skotch_ast::KtFun<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> MethodSig {
    let params: Vec<Ty> = f
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
    let ret = f
        .return_type()
        .map(|tr| type_ref_to_ty(tr, imports, aliases))
        .unwrap_or_else(|| infer_return_ty(f));
    MethodSig {
        name: f.name().unwrap_or("").to_string(),
        params,
        ret,
    }
}

fn register_class(
    c: KtClass<'_>,
    env: &mut TypeEnv,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    let Some(name) = c.name() else { return };
    let (body_methods, body_props) = class_body_methods_props(c.body());
    let mut fields: Vec<FieldSig> = body_props
        .iter()
        .map(|p| FieldSig {
            name: p.name().unwrap_or("").to_string(),
            ty: p
                .type_reference()
                .map(|tr| type_ref_to_ty(tr, imports, aliases))
                .unwrap_or(Ty::Any),
        })
        .collect();
    // Primary-constructor val/var params become fields too.
    if let Some(pc) = c.primary_constructor() {
        if let Some(plist) = pc.value_parameter_list() {
            for p in plist.parameters() {
                if p.is_val() || p.is_var() {
                    if let Some(pname) = p.name() {
                        let ty = p
                            .type_reference()
                            .map(|tr| type_ref_to_ty(tr, imports, aliases))
                            .unwrap_or(Ty::Any);
                        fields.push(FieldSig {
                            name: pname.to_string(),
                            ty,
                        });
                    }
                }
            }
        }
    }
    let methods: Vec<MethodSig> = body_methods
        .iter()
        .map(|f| method_sig_from_fun(*f, imports, aliases))
        .collect();
    let (companion_methods, _) = companion_signatures(c.body(), imports, aliases);
    let (super_class, interfaces) = collect_super_class_iface(c.super_type_list());

    if c.is_sealed() {
        // Track this as a sealed parent; subclass tracking happens
        // when their `super_class` matches.
        env.sealed_subclasses.entry(name.to_string()).or_default();
    }
    // Add as a subclass to the parent's sealed list if the parent is sealed.
    if let Some(sc) = &super_class {
        if env.types.get(sc).map(|d| d.is_sealed).unwrap_or(false) {
            env.sealed_subclasses
                .entry(sc.clone())
                .or_default()
                .push(name.to_string());
        }
    }
    env.types.insert(
        name.to_string(),
        TypeDecl {
            name: name.to_string(),
            super_class,
            interfaces,
            fields,
            methods,
            companion_methods,
            is_enum: false,
            enum_entry_names: Vec::new(),
            is_sealed: c.is_sealed(),
        },
    );
}

fn register_interface(
    i: KtInterface<'_>,
    env: &mut TypeEnv,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    let Some(name) = i.name() else { return };
    let (body_methods, _) = class_body_methods_props(i.body());
    let methods: Vec<MethodSig> = body_methods
        .iter()
        .map(|f| method_sig_from_fun(*f, imports, aliases))
        .collect();
    let (_super, interfaces) = collect_super_class_iface(i.super_type_list());
    if i.is_sealed() {
        env.sealed_subclasses.entry(name.to_string()).or_default();
    }
    env.types.insert(
        name.to_string(),
        TypeDecl {
            name: name.to_string(),
            super_class: None,
            interfaces,
            fields: Vec::new(),
            methods,
            companion_methods: Vec::new(),
            is_enum: false,
            enum_entry_names: Vec::new(),
            is_sealed: i.is_sealed(),
        },
    );
}

fn register_object(
    o: KtObjectDeclaration<'_>,
    env: &mut TypeEnv,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    let Some(name) = o.name() else { return };
    let (body_methods, body_props) = class_body_methods_props(o.body());
    let fields: Vec<FieldSig> = body_props
        .iter()
        .map(|p| FieldSig {
            name: p.name().unwrap_or("").to_string(),
            ty: p
                .type_reference()
                .map(|tr| type_ref_to_ty(tr, imports, aliases))
                .unwrap_or(Ty::Any),
        })
        .collect();
    let methods: Vec<MethodSig> = body_methods
        .iter()
        .map(|f| method_sig_from_fun(*f, imports, aliases))
        .collect();
    let (super_class, interfaces) = collect_super_class_iface(o.super_type_list());
    env.types.insert(
        name.to_string(),
        TypeDecl {
            name: name.to_string(),
            super_class,
            interfaces,
            fields,
            methods,
            companion_methods: Vec::new(),
            is_enum: false,
            enum_entry_names: Vec::new(),
            is_sealed: false,
        },
    );
}

fn register_enum(
    e: KtEnumClass<'_>,
    env: &mut TypeEnv,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) {
    let Some(name) = e.name() else { return };
    let (body_methods, body_props) = class_body_methods_props(e.body());
    let fields: Vec<FieldSig> = body_props
        .iter()
        .map(|p| FieldSig {
            name: p.name().unwrap_or("").to_string(),
            ty: p
                .type_reference()
                .map(|tr| type_ref_to_ty(tr, imports, aliases))
                .unwrap_or(Ty::Any),
        })
        .collect();
    let methods: Vec<MethodSig> = body_methods
        .iter()
        .map(|f| method_sig_from_fun(*f, imports, aliases))
        .collect();
    let enum_entry_names: Vec<String> = e
        .body()
        .map(|b| {
            b.enum_entries()
                .filter_map(|ee| ee.name().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for entry in &enum_entry_names {
        env.enum_entries.insert(entry.clone(), name.to_string());
    }
    env.types.insert(
        name.to_string(),
        TypeDecl {
            name: name.to_string(),
            super_class: Some("kotlin/Enum".to_string()),
            interfaces: Vec::new(),
            fields,
            methods,
            companion_methods: Vec::new(),
            is_enum: true,
            enum_entry_names,
            is_sealed: false,
        },
    );
}

// ── Top-val cycle detection ─────────────────────────────────────────

/// Detect circular references among top-level val initializers.
/// Emits a diagnostic for each cycle, matching legacy
/// `crate::detect_top_val_cycles` semantics.
fn detect_top_val_cycles(file: KtFile<'_>, _diags: &mut Diagnostics) {
    use rustc_hash::FxHashSet;
    // Build name → initializer refs map.
    let mut refs: FxHashMap<String, Vec<String>> = FxHashMap::default();
    for d in file.decls() {
        if let KtDecl::Property(p) = d {
            if let Some(name) = p.name() {
                let mut found = Vec::new();
                if let Some(init) = p.initializer() {
                    collect_top_val_refs(&init, &mut found);
                }
                refs.insert(name.to_string(), found);
            }
        }
    }
    // DFS for cycles. A cycle path is collected and reported.
    fn dfs(
        node: &str,
        refs: &FxHashMap<String, Vec<String>>,
        visited: &mut FxHashSet<String>,
        path: &mut Vec<String>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        if path.iter().any(|n| n == node) {
            let pos = path.iter().position(|n| n == node).unwrap();
            let cycle: Vec<String> = path[pos..].to_vec();
            cycles.push(cycle);
            return;
        }
        if !visited.insert(node.to_string()) {
            return;
        }
        path.push(node.to_string());
        if let Some(rs) = refs.get(node) {
            for r in rs {
                if refs.contains_key(r) {
                    dfs(r, refs, visited, path, cycles);
                }
            }
        }
        path.pop();
    }
    let mut visited = FxHashSet::default();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    for name in refs.keys() {
        let mut path = Vec::new();
        dfs(name, &refs, &mut visited, &mut path, &mut cycles);
    }
    // diags integration pending — for now, callers reading TypedFile
    // don't yet see the cycle errors. When wired through, emit a
    // Diagnostic::Error per cycle.
    let _ = cycles;
}

fn collect_top_val_refs(e: &skotch_ast::KtExpr<'_>, sink: &mut Vec<String>) {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Reference(r) => {
            if let Some(name) = r.name() {
                sink.push(name.to_string());
            }
        }
        KtExpr::Binary(b) => {
            if let Some(l) = b.lhs() {
                collect_top_val_refs(&l, sink);
            }
            if let Some(r) = b.rhs() {
                collect_top_val_refs(&r, sink);
            }
        }
        KtExpr::Parenthesized(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    collect_top_val_refs(&inner, sink);
                }
            }
        }
        _ => {
            for c in skotch_ast::children(e.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    collect_top_val_refs(&inner, sink);
                }
            }
        }
    }
}

fn companion_signatures(
    body: Option<KtClassBody<'_>>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> (Vec<MethodSig>, bool) {
    let mut methods = Vec::new();
    let mut has_companion = false;
    if let Some(b) = body {
        for d in b.declarations() {
            if let KtDecl::Object(o) = d {
                if o.is_companion() {
                    has_companion = true;
                    let (body_methods, _) = class_body_methods_props(o.body());
                    for f in body_methods {
                        methods.push(method_sig_from_fun(f, imports, aliases));
                    }
                }
            }
        }
    }
    (methods, has_companion)
}

/// Walk a function body to harvest local variable types in source
/// order. Local `val`/`var` declarations surface as PROPERTY children
/// of the BLOCK composite; nested blocks (inside `if` / `when` arms)
/// recurse. Synthesized expression types feed the scope so a later
/// initializer that references an earlier local picks up its type.
#[allow(clippy::too_many_arguments)]
fn walk_block_with_scope_and_fns(
    block: skotch_ast::KtBlock<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
    fn_returns: &FxHashMap<String, Ty>,
    env: &TypeEnv,
    local_tys: &mut Vec<Ty>,
    scope: &mut Vec<(String, Ty)>,
) {
    use skotch_ast::KtExpr;
    let saved = scope.len();
    for c in skotch_ast::children(block.syntax()) {
        if let Some(prop) = skotch_ast::KtProperty::cast(c) {
            let ty = prop
                .type_reference()
                .map(|tr| type_ref_to_ty(tr, imports, aliases))
                .or_else(|| {
                    prop.initializer()
                        .map(|e| synth_expr(&e, scope, fn_returns, env))
                })
                .unwrap_or(Ty::Any);
            local_tys.push(ty.clone());
            if let Some(name) = prop.name() {
                scope.push((name.to_string(), ty));
            }
            continue;
        }
        if let Some(expr) = KtExpr::cast(c) {
            match expr {
                KtExpr::If(i) => {
                    if let Some(KtExpr::Block(b)) = i.then_branch().and_then(|t| t.expression()) {
                        walk_block_with_scope_and_fns(
                            b, imports, aliases, fn_returns, env, local_tys, scope,
                        );
                    }
                    if let Some(KtExpr::Block(b)) = i.else_branch().and_then(|e| e.expression()) {
                        walk_block_with_scope_and_fns(
                            b, imports, aliases, fn_returns, env, local_tys, scope,
                        );
                    }
                }
                KtExpr::Block(b) => walk_block_with_scope_and_fns(
                    b, imports, aliases, fn_returns, env, local_tys, scope,
                ),
                _ => {}
            }
        }
    }
    scope.truncate(saved);
}

/// Synthesize the type of an expression against the given scope, the
/// top-level fn_returns map, and the TypeEnv (class/iface/enum/object
/// members). Mirrors the legacy `TypeChecker::synth_expr` for the
/// common shapes. Member calls and field access through DotQualified
/// expressions are resolved against the TypeEnv.
fn synth_expr(
    e: &skotch_ast::KtExpr<'_>,
    scope: &[(String, Ty)],
    fn_returns: &FxHashMap<String, Ty>,
    env: &TypeEnv,
) -> Ty {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Boolean(_) => Ty::Bool,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Float(_) => Ty::Double,
        KtExpr::Character(_) => Ty::Char,
        KtExpr::Null(_) => Ty::Nullable(Box::new(Ty::Any)),
        KtExpr::String(_) => Ty::String,
        KtExpr::Reference(r) => {
            if let Some(name) = r.name() {
                if let Some((_, t)) = scope.iter().rev().find(|(n, _)| n == name) {
                    return t.clone();
                }
                // Enum entry referenced bare: `RED` (in a context where
                // its enum is implied) → Ty::Class(EnumName).
                if let Some(enum_name) = env.enum_entries.get(name) {
                    return Ty::Class(enum_name.clone());
                }
                // Top-level user class name as a value: `MyClass` → the
                // class type itself. Useful for Companion access.
                if env.types.contains_key(name) {
                    return Ty::Class(name.to_string());
                }
            }
            Ty::Any
        }
        KtExpr::Parenthesized(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope, fn_returns, env);
                }
            }
            Ty::Any
        }
        KtExpr::Binary(b) => {
            let lt = b
                .lhs()
                .map(|l| synth_expr(&l, scope, fn_returns, env))
                .unwrap_or(Ty::Any);
            let rt = b
                .rhs()
                .map(|r| synth_expr(&r, scope, fn_returns, env))
                .unwrap_or(Ty::Any);
            let op = b.operation().map(|o| o.text()).unwrap_or_default();
            match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                "+" | "-" | "*" | "/" | "%" => {
                    if lt == Ty::Double || rt == Ty::Double {
                        Ty::Double
                    } else if lt == Ty::Long || rt == Ty::Long {
                        Ty::Long
                    } else if matches!(lt, Ty::Int | Ty::Any) && matches!(rt, Ty::Int | Ty::Any) {
                        Ty::Int
                    } else if op == "+" && lt == Ty::String {
                        Ty::String
                    } else if let Ty::Class(class_name) = &lt {
                        // Operator overloading: receiver.plus(rhs)
                        let op_method = match op.as_str() {
                            "+" => "plus",
                            "-" => "minus",
                            "*" => "times",
                            "/" => "div",
                            "%" => "rem",
                            _ => unreachable!(),
                        };
                        if let Some(m) = env.lookup_method(class_name, op_method) {
                            // Unit return on operator body → assume
                            // receiver type (legacy behavior).
                            if m.ret == Ty::Unit {
                                lt.clone()
                            } else {
                                m.ret.clone()
                            }
                        } else {
                            Ty::Int
                        }
                    } else {
                        Ty::Int
                    }
                }
                _ => Ty::Any,
            }
        }
        KtExpr::Unary(u) => {
            for c in skotch_ast::children(u.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope, fn_returns, env);
                }
            }
            Ty::Any
        }
        KtExpr::Prefix(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope, fn_returns, env);
                }
            }
            Ty::Any
        }
        KtExpr::Postfix(p) => {
            for c in skotch_ast::children(p.syntax()) {
                if let Some(inner) = KtExpr::cast(c) {
                    return synth_expr(&inner, scope, fn_returns, env);
                }
            }
            Ty::Any
        }
        KtExpr::Call(call) => {
            // A bare Call (not nested in a DotQualified).
            //   helper()  — top-level fn → fn_returns
            //   Box(7)    — class constructor → Ty::Class(Box)
            if let Some(KtExpr::Reference(r)) = call.callee() {
                if let Some(name) = r.name() {
                    if let Some(ret) = fn_returns.get(name) {
                        return ret.clone();
                    }
                    if env.types.contains_key(name) {
                        return Ty::Class(name.to_string());
                    }
                }
            }
            Ty::Any
        }
        KtExpr::DotQualified(dq) => {
            // `lhs.rhs` — rhs may be a Reference (field/property access)
            // or a Call (member method call).
            resolve_dot_qualified(*dq, scope, fn_returns, env)
        }
        // KtExpr::Lambda / KtExpr::Try / KtExpr::Throw etc. — Ty::Any until ported.
        _ => Ty::Any,
    }
}

/// Resolve a `receiver.member` expression where `member` is either a
/// Reference (field) or a Call (method). The SIL emits both as direct
/// children of the DOT_QUALIFIED_EXPRESSION composite.
fn resolve_dot_qualified(
    dq: skotch_ast::KtDotQualifiedExpression<'_>,
    scope: &[(String, Ty)],
    fn_returns: &FxHashMap<String, Ty>,
    env: &TypeEnv,
) -> Ty {
    use skotch_ast::KtExpr;
    let children: Vec<_> = skotch_ast::children(dq.syntax())
        .iter()
        .filter_map(KtExpr::cast)
        .collect();
    if children.len() < 2 {
        return Ty::Any;
    }
    let receiver = &children[0];
    let rhs = &children[1];
    let receiver_ty = synth_expr(receiver, scope, fn_returns, env);
    match rhs {
        KtExpr::Reference(r) => {
            let Some(name) = r.name() else { return Ty::Any };
            field_or_enum_entry(&receiver_ty, name, env)
        }
        KtExpr::Call(call) => {
            // Method call. Method name is the call's callee Reference.
            let method_name = match call.callee() {
                Some(KtExpr::Reference(rc)) => rc.name(),
                _ => None,
            };
            let Some(method_name) = method_name else {
                return Ty::Any;
            };
            method_on_receiver(&receiver_ty, method_name, env)
        }
        _ => Ty::Any,
    }
}

fn field_or_enum_entry(receiver_ty: &Ty, name: &str, env: &TypeEnv) -> Ty {
    if let Ty::Class(class_name) = receiver_ty {
        if let Some(decl) = env.types.get(class_name) {
            if decl.is_enum && decl.enum_entry_names.iter().any(|e| e == name) {
                return Ty::Class(class_name.clone());
            }
            if let Some(f) = decl.fields.iter().find(|f| f.name == name) {
                return f.ty.clone();
            }
        }
        if let Some(f) = env.lookup_field(class_name, name) {
            return f.ty.clone();
        }
    }
    Ty::Any
}

fn method_on_receiver(receiver_ty: &Ty, name: &str, env: &TypeEnv) -> Ty {
    if let Ty::Class(class_name) = receiver_ty {
        if let Some(m) = env.lookup_companion(class_name, name) {
            return m.ret.clone();
        }
        if let Some(m) = env.lookup_method(class_name, name) {
            return m.ret.clone();
        }
    }
    Ty::Any
}

// ── Import collection ───────────────────────────────────────────────

fn collect_imports(file: KtFile<'_>) -> FxHashMap<String, String> {
    let mut out = FxHashMap::default();
    if let Some(list) = file.import_list() {
        for imp in skotch_ast::typed_children::<skotch_ast::KtImportDirective>(list.syntax()) {
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
                out.insert(simple.to_string(), fq);
            }
        }
    }
    out
}

// ── Type-ref → Ty (typed, with alias side table) ────────────────────

#[derive(Clone)]
struct AliasTarget {
    /// SilNode pointer of the alias target's TYPE_REFERENCE. Lifetime
    /// is bounded by the enclosing `ParsedFile`'s pin.
    target_node_ptr: usize,
}

impl AliasTarget {
    fn from_type_ref(tr: KtTypeReference<'_>) -> Self {
        Self {
            target_node_ptr: tr.syntax() as *const _ as usize,
        }
    }
    fn as_type_ref<'a>(&self) -> KtTypeReference<'a> {
        let raw = self.target_node_ptr as *const skotch_sil::SilNode;
        let node = unsafe { &*raw };
        KtTypeReference::cast(node).expect("alias target stored as TYPE_REFERENCE")
    }
}

fn type_ref_to_ty(
    tr: KtTypeReference<'_>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Ty {
    if let Some(ft) = tr.function_type() {
        return function_type_to_ty(ft, imports, aliases, tr.is_suspend(), tr.is_composable());
    }
    if let Some(n) = tr.nullable_type() {
        let inner = if let Some(u) = n.inner_user_type() {
            user_type_to_ty(u, imports, aliases)
        } else if let Some(ft) = n.inner_function_type() {
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
        return type_ref_to_ty(target.as_type_ref(), imports, aliases);
    }
    skotch_types::ty_from_name(name).unwrap_or_else(|| {
        if let Some(jvm) = skotch_types::intrinsics::kotlin_to_jvm_class(name) {
            Ty::Class(jvm.to_string())
        } else if let Some(fq) = imports.get(name) {
            Ty::Class(fq.clone())
        } else if name.starts_with(char::is_uppercase) {
            // Capitalized name without explicit import → assume
            // same-package class. Without this, fn params of
            // user-class type (`fun statsOf(e: Expr): String`)
            // collapsed to `Ty::Any`, and the JVM call site emitted
            // `(LObject;)String` instead of `(LExpr;)String` —
            // NoSuchMethodError at runtime.
            Ty::Class(name.to_string())
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
    let mut params: Vec<Ty> = Vec::new();
    // Extension function type `T.() -> R` desugars to `Function1<T, R>`
    // on the JVM — the receiver becomes the first param. Without
    // prepending it here, a `fun html(init: HTML.() -> Unit)` declaration
    // surfaces `init` as `Function0` and the call-site `html { ... }`
    // dispatch builds a `(Function0)HTML` descriptor against the real
    // `(Function1)HTML` runtime shape.
    if let Some(recv) = ft.receiver() {
        if let Some(rtr) = recv.type_reference() {
            params.push(type_ref_to_ty(rtr, imports, aliases));
        } else if let Some(u) =
            skotch_ast::first_typed_child::<skotch_ast::KtUserType<'_>>(recv.syntax())
        {
            params.push(user_type_to_ty(u, imports, aliases));
        }
    }
    if let Some(pl) = ft.parameter_list() {
        for p in pl.parameters() {
            params.push(
                p.type_reference()
                    .map(|ptr| type_ref_to_ty(ptr, imports, aliases))
                    .unwrap_or(Ty::Any),
            );
        }
    }
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

fn collect_param_tys(
    plist: Option<KtValueParameterList<'_>>,
    imports: &FxHashMap<String, String>,
    aliases: &FxHashMap<String, AliasTarget>,
) -> Vec<Ty> {
    plist
        .map(|pl| {
            pl.parameters()
                .map(|p: KtValueParameter<'_>| {
                    p.type_reference()
                        .map(|tr| type_ref_to_ty(tr, imports, aliases))
                        .unwrap_or(Ty::Any)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Best-effort return-type inference from a function with no explicit
/// `: Type` annotation. Mirrors the legacy `infer_body_return_ty`
/// semantics — only when an explicit `return value` statement is
/// present do we narrow.
fn infer_return_ty(f: KtFun<'_>) -> Ty {
    use skotch_ast::KtExpr;
    if let Some(e) = f.body_expression() {
        return literal_ty(&e);
    }
    let Some(block) = f.body_block() else {
        return Ty::Unit;
    };
    let mut returned: Option<KtExpr<'_>> = None;
    for stmt in block.statements() {
        if let KtExpr::Return(r) = stmt {
            for c in skotch_ast::children(r.syntax()) {
                if let Some(e) = KtExpr::cast(c) {
                    returned = Some(e);
                }
            }
        }
    }
    returned.map(|e| literal_ty(&e)).unwrap_or(Ty::Unit)
}

fn literal_ty(e: &skotch_ast::KtExpr<'_>) -> Ty {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Boolean(_) => Ty::Bool,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Float(_) => Ty::Double,
        KtExpr::Character(_) => Ty::Char,
        KtExpr::Null(_) => Ty::Nullable(Box::new(Ty::Any)),
        KtExpr::String(_) => Ty::String,
        KtExpr::Binary(b) => {
            let op = b.operation().map(|o| o.text()).unwrap_or_default();
            match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                _ => Ty::Any,
            }
        }
        // Pass through parenthesized expressions transparently.
        KtExpr::Parenthesized(p) => skotch_ast::children(p.syntax())
            .iter()
            .find_map(KtExpr::cast)
            .map(|inner| literal_ty(&inner))
            .unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_type_check_finds_top_level_fun() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions.len(), 1);
        assert_eq!(typed.functions[0].name_index, 0);
        assert!(matches!(typed.functions[0].return_ty, Ty::Unit));
    }

    #[test]
    fn typed_type_check_collects_param_count() {
        let parsed = skotch_ast::parse("test.kt", "fun add(a: Int, b: Int): Int = a + b");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions.len(), 1);
        assert_eq!(typed.functions[0].param_tys, vec![Ty::Int, Ty::Int]);
        assert_eq!(typed.functions[0].return_ty, Ty::Int);
    }

    #[test]
    fn typed_type_check_registers_signatures_by_def_id() {
        let parsed = skotch_ast::parse("test.kt", "fun a() {}\nfun b() {}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert!(typed.top_signatures.contains_key(&DefId::Function(0)));
        assert!(typed.top_signatures.contains_key(&DefId::Function(1)));
    }

    #[test]
    fn typed_string_param_resolves_to_string_ty() {
        let parsed = skotch_ast::parse("test.kt", "fun greet(name: String): String = name");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions[0].param_tys, vec![Ty::String]);
        assert_eq!(typed.functions[0].return_ty, Ty::String);
    }

    #[test]
    fn typed_nullable_returns_nullable() {
        let parsed = skotch_ast::parse("test.kt", "fun maybe(x: Int?): String? = null");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert!(matches!(typed.functions[0].param_tys[0], Ty::Nullable(_)));
        assert!(matches!(typed.functions[0].return_ty, Ty::Nullable(_)));
    }

    #[test]
    fn typed_top_val_recorded() {
        let parsed = skotch_ast::parse("test.kt", "val GREETING: String = \"hi\"");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.top_vals.len(), 1);
        assert_eq!(typed.top_vals[0].ty, Ty::String);
        assert!(typed.top_signatures.contains_key(&DefId::TopLevelVal(0)));
    }

    #[test]
    fn typealias_substitution_to_function_ty() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "typealias Predicate = (Int) -> Boolean\nfun apply(p: Predicate): Boolean = true",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        match &typed.functions[0].param_tys[0] {
            Ty::Function { params, ret, .. } => {
                assert_eq!(params.as_slice(), &[Ty::Int]);
                assert_eq!(**ret, Ty::Bool);
            }
            other => panic!("expected Function, got {other:?}"),
        }
    }

    #[test]
    fn expr_body_literal_infers_return_ty() {
        let parsed = skotch_ast::parse("test.kt", "fun pi() = 3.14");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert_eq!(typed.functions[0].return_ty, Ty::Double);
    }

    #[test]
    fn body_walks_record_local_types() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a: Int = 1\n  val b: String = \"hi\"\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::String]);
    }

    #[test]
    fn body_locals_infer_from_initializer_when_no_annotation() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {\n  val a = 42\n}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int]);
    }

    #[test]
    fn body_local_initialized_from_binary_op() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {\n  val a = 1\n  val b = a + 2\n}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        // a inferred from literal as Int; b from synth_expr(a + 2) → Int.
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn body_local_initialized_from_comparison() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {\n  val a = 1\n  val b = a > 0\n}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::Int, Ty::Bool]);
    }

    #[test]
    fn body_local_resolves_fn_call_return_type() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun helper(): Int = 1\nfun main() {\n  val x = helper()\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let main_fn = typed
            .functions
            .iter()
            .find(|f| f.name_index == 1)
            .expect("main fn");
        // x = helper() — helper is declared returning Int.
        assert_eq!(main_fn.local_tys, vec![Ty::Int]);
    }

    #[test]
    fn body_local_resolves_param_type() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun process(s: String) {\n  val x = s\n  val y = x + \"!\"\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        // x = s (param, String); y = x + "!" (String + String = String).
        assert_eq!(f.local_tys, vec![Ty::String, Ty::String]);
    }

    #[test]
    fn body_local_resolves_member_call() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "class Box(val x: Int) { fun stretch(): Int = 1 }\nfun main() {\n  val b = Box(7)\n  val s = b.stretch()\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let main_fn = typed
            .functions
            .iter()
            .find(|f| f.name_index == 0)
            .expect("main");
        // b → Ty::Class("Box") (constructor call); s → b.stretch() → Int.
        assert_eq!(
            main_fn.local_tys,
            vec![Ty::Class("Box".to_string()), Ty::Int],
        );
    }

    #[test]
    fn body_local_resolves_field_access() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "class P(val x: String)\nfun touch(p: P) { val v = p.x }",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = typed
            .functions
            .iter()
            .find(|f| f.name_index == 0)
            .expect("touch");
        // p.x → field lookup on Ty::Class(P) returns String.
        assert_eq!(f.local_tys, vec![Ty::String]);
    }

    #[test]
    fn body_local_resolves_enum_entry_access() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "enum class Color { RED, GREEN, BLUE }\nfun main() { val c = Color.RED }",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let main_fn = typed
            .functions
            .iter()
            .find(|f| f.name_index == 0)
            .expect("main");
        // Color.RED → Ty::Class("Color").
        match &main_fn.local_tys[0] {
            Ty::Class(n) => assert_eq!(n, "Color"),
            other => panic!("expected Class(Color), got {other:?}"),
        }
    }

    #[test]
    fn body_local_initialized_from_string_concat() {
        let parsed = skotch_ast::parse(
            "test.kt",
            "fun main() {\n  val a = \"hi\"\n  val b = a + \"!\"\n}",
        );
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        let f = &typed.functions[0];
        assert_eq!(f.local_tys, vec![Ty::String, Ty::String]);
    }
}
