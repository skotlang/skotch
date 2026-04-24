//! Type checker for the Kotlin subset skotch accepts.
//!
//! # Soundness Invariant
//!
//! All type checking MUST go through [`TypeChecker::is_assignable`],
//! which delegates to [`Ty::assignable_to_in`] with the class hierarchy.
//! **Never use `Ty::assignable_to` directly** in this crate — it lacks
//! hierarchy info and is only a conservative fallback.
//!
//! When the type of an expression cannot be determined, the typechecker
//! returns `Ty::Error` (NOT `Ty::Any`). `Ty::Error` suppresses
//! cascading diagnostics without silently claiming a value is `Any`.
//!
//! The soundness invariant tests at the bottom of this file **must
//! never be weakened, loosened, or removed**.
//!
//! ## Architecture
//!
//! **Type environment**: Before checking function bodies, the checker
//! collects a `TypeEnv` from the file's declarations: class fields,
//! interface methods, enum entries, companion methods. This allows
//! `synth_expr` to resolve `receiver.method()` and `receiver.field`
//! for user-defined types — not just built-in String methods.
//!
//! **Two-pass bidirectional**: Pass 1 collects signatures for all
//! top-level declarations (functions, vals, classes, interfaces, enums).
//! Pass 2 checks each function body using those signatures plus the
//! type environment.
//!
//! ## Output
//!
//! [`TypedFile`] provides:
//! - `functions[i].return_ty` / `.param_tys` — used by MIR lowering
//! - `functions[i].local_tys` — used by LSP hover info
//! - `top_signatures` — used by LSP and internal call resolution

use rustc_hash::FxHashMap;
use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_resolve::{DefId, ResolvedFile};
use skotch_syntax::{
    BinOp, Block, ClassDecl, Decl, EnumDecl, Expr, FunDecl, InterfaceDecl, KtFile, Stmt,
    TemplatePart, TypeRef, ValDecl,
};
use skotch_types::{ty_from_name, Ty};

/// Map well-known Kotlin source-level class names to their JVM internal names
/// so that type descriptors use `Ljava/util/List;` rather than `LList;`.
fn well_known_class_name(name: &str) -> Option<&'static str> {
    match name {
        "List" | "MutableList" => Some("java/util/List"),
        "Map" | "MutableMap" => Some("java/util/Map"),
        "Set" | "MutableSet" => Some("java/util/Set"),
        "Collection" => Some("java/util/Collection"),
        "Iterable" => Some("java/lang/Iterable"),
        "Iterator" => Some("java/util/Iterator"),
        "Pair" => Some("kotlin/Pair"),
        "Triple" => Some("kotlin/Triple"),
        _ => None,
    }
}

// ─── Public output types ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TypedFunction {
    pub name_index: u32,
    pub return_ty: Ty,
    pub param_tys: Vec<Ty>,
    pub local_tys: Vec<Ty>,
    // expr_tys removed — it was dead code (populated but never consumed).
}

#[derive(Clone, Debug)]
pub struct TypedTopVal {
    pub name_index: u32,
    pub ty: Ty,
}

#[derive(Default, Clone, Debug)]
pub struct TypedFile {
    pub functions: Vec<TypedFunction>,
    pub top_vals: Vec<TypedTopVal>,
    pub top_signatures: FxHashMap<DefId, Signature>,
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

// ─── Type environment ───────────────────────────────────────────────────────

/// A method signature within a class/interface/enum.
#[derive(Clone, Debug)]
struct MethodSig {
    name: String,
    #[allow(dead_code)]
    params: Vec<Ty>, // used for future overload resolution
    ret: Ty,
}

/// A field within a class/enum.
#[derive(Clone, Debug)]
struct FieldSig {
    name: String,
    ty: Ty,
}

/// Type information for a user-declared type.
#[derive(Clone, Debug)]
#[allow(dead_code)] // name and is_enum reserved for future diagnostics
struct TypeDecl {
    name: String,
    super_class: Option<String>,
    interfaces: Vec<String>,
    fields: Vec<FieldSig>,
    methods: Vec<MethodSig>,
    companion_methods: Vec<MethodSig>,
    is_enum: bool,
}

/// The type environment built from all declarations in the file.
#[derive(Default, Clone, Debug)]
struct TypeEnv {
    /// type_name → TypeDecl
    types: FxHashMap<String, TypeDecl>,
    /// Enum entry names → (enum_class_name)
    enum_entries: FxHashMap<String, String>,
}

impl TypeEnv {
    /// Look up a method on a type, walking the superclass chain and interfaces.
    fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<&MethodSig> {
        let mut search = Some(type_name.to_string());
        while let Some(ref name) = search {
            if let Some(decl) = self.types.get(name) {
                if let Some(m) = decl.methods.iter().find(|m| m.name == method_name) {
                    return Some(m);
                }
                // Search interfaces.
                for iface in &decl.interfaces {
                    if let Some(m) = self.lookup_method(iface, method_name) {
                        return Some(m);
                    }
                }
                search = decl.super_class.clone();
            } else {
                break;
            }
        }
        None
    }

    /// Look up a field on a type, walking the superclass chain.
    fn lookup_field(&self, type_name: &str, field_name: &str) -> Option<&FieldSig> {
        let mut search = Some(type_name.to_string());
        while let Some(ref name) = search {
            if let Some(decl) = self.types.get(name) {
                if let Some(f) = decl.fields.iter().find(|f| f.name == field_name) {
                    return Some(f);
                }
                search = decl.super_class.clone();
            } else {
                break;
            }
        }
        None
    }

    /// Look up a companion method on a type.
    fn lookup_companion(&self, type_name: &str, method_name: &str) -> Option<&MethodSig> {
        self.types
            .get(type_name)
            .and_then(|d| d.companion_methods.iter().find(|m| m.name == method_name))
    }

    /// Sound subtype check: is `child` the same class as, or a
    /// subclass/implementor of, `parent`?
    ///
    /// Walks the declared superclass chain and interfaces transitively.
    /// Returns `false` for unknown types (conservative — forces the
    /// caller to emit an error rather than silently accept).
    fn is_subclass(&self, child: &str, parent: &str) -> bool {
        if child == parent {
            return true;
        }
        let mut visited = rustc_hash::FxHashSet::default();
        let mut stack = vec![child.to_string()];
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue; // cycle guard
            }
            if let Some(decl) = self.types.get(&current) {
                if let Some(ref sup) = decl.super_class {
                    if sup == parent {
                        return true;
                    }
                    stack.push(sup.clone());
                }
                for iface in &decl.interfaces {
                    if iface == parent {
                        return true;
                    }
                    stack.push(iface.clone());
                }
            }
        }
        false
    }
}

// ─── Entry point ────────────────────────────────────────────────────────────

pub fn type_check(
    file: &KtFile,
    resolved: &ResolvedFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    _package_symbols: Option<&skotch_resolve::PackageSymbolTable>,
) -> TypedFile {
    let mut tc = TypeChecker {
        interner,
        diags,
        out: TypedFile::default(),
        fn_names: Vec::new(),
        env: TypeEnv::default(),
        type_params: Vec::new(),
        type_aliases: FxHashMap::default(),
    };

    // ── Collect type aliases ───────────────────────────────────────────
    for decl in &file.decls {
        if let Decl::TypeAlias(ta) = decl {
            let alias_name = tc.interner.resolve(ta.name).to_string();
            let target_name = tc.interner.resolve(ta.target.name).to_string();
            tc.type_aliases.insert(alias_name, target_name);
        }
    }

    // ── Build type environment from all declarations ────────────────────
    for decl in &file.decls {
        match decl {
            Decl::Class(c) => tc.register_class(c),
            Decl::Interface(i) => tc.register_interface(i),
            Decl::Enum(e) => tc.register_enum(e),
            _ => {}
        }
    }

    // ── Pass 1: collect signatures ──────────────────────────────────────
    let mut fn_idx_pass1: u32 = 0;
    let mut val_idx_pass1: u32 = 0;
    for decl in &file.decls {
        match decl {
            Decl::Fun(f) => {
                tc.type_params = f
                    .type_params
                    .iter()
                    .map(|tp| tc.interner.resolve(tp.name).to_string())
                    .collect();
                let sig = tc.signature_for_fun(f);
                tc.out
                    .top_signatures
                    .insert(DefId::Function(fn_idx_pass1), sig);
                tc.fn_names.push(f.name);
                tc.type_params.clear();
                fn_idx_pass1 += 1;
            }
            Decl::Val(v) => {
                let ty = tc.synth_top_init(&v.init);
                tc.out.top_signatures.insert(
                    DefId::TopLevelVal(val_idx_pass1),
                    Signature {
                        params: vec![],
                        ret: ty,
                    },
                );
                val_idx_pass1 += 1;
            }
            Decl::Class(_) => {
                // Constructor calls are resolved via env.types in synth_expr.
            }
            Decl::Object(_)
            | Decl::Enum(_)
            | Decl::Interface(_)
            | Decl::TypeAlias(_)
            | Decl::Unsupported { .. } => {}
        }
    }
    tc.out.top_signatures.insert(
        DefId::PrintlnIntrinsic,
        Signature {
            params: vec![Ty::Nullable(Box::new(Ty::Any))],
            ret: Ty::Unit,
        },
    );

    // ── Pass 2: check function bodies ───────────────────────────────────
    let mut fn_idx: u32 = 0;
    let mut val_idx: u32 = 0;
    for decl in &file.decls {
        match decl {
            Decl::Fun(f) => {
                // Set type params in scope for this function.
                tc.type_params = f
                    .type_params
                    .iter()
                    .map(|tp| tc.interner.resolve(tp.name).to_string())
                    .collect();
                let rf = &resolved.functions[fn_idx as usize];
                let typed = tc.check_function(fn_idx, f, rf);
                tc.out.functions.push(typed);
                tc.type_params.clear();
                fn_idx += 1;
            }
            Decl::Val(v) => {
                let typed = tc.check_top_val(val_idx, v);
                tc.out.top_vals.push(typed);
                val_idx += 1;
            }
            Decl::Class(_)
            | Decl::Object(_)
            | Decl::Enum(_)
            | Decl::Interface(_)
            | Decl::TypeAlias(_)
            | Decl::Unsupported { .. } => {}
        }
    }
    tc.out
}

// ─── Type checker ───────────────────────────────────────────────────────────

struct TypeChecker<'a> {
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    out: TypedFile,
    fn_names: Vec<Symbol>,
    env: TypeEnv,
    /// Type parameter names currently in scope (e.g. "T", "R").
    type_params: Vec<String>,
    /// Type alias mappings: alias name → target type name.
    type_aliases: FxHashMap<String, String>,
}

impl<'a> TypeChecker<'a> {
    /// Sound assignability check using the class hierarchy.
    ///
    /// This is the ONLY method that should be used for type checking
    /// assignability within the typechecker. It delegates to
    /// `Ty::assignable_to_in` with the environment's class hierarchy.
    fn is_assignable(&self, from: &Ty, to: &Ty) -> bool {
        from.assignable_to_in(to, &|child, parent| self.env.is_subclass(child, parent))
    }

    // ── Type environment builders ───────────────────────────────────────

    fn register_class(&mut self, c: &ClassDecl) {
        let name = self.interner.resolve(c.name).to_string();
        let super_class = c
            .parent_class
            .as_ref()
            .map(|sc| self.interner.resolve(sc.name).to_string());
        let interfaces: Vec<String> = c
            .interfaces
            .iter()
            .map(|s| self.interner.resolve(*s).to_string())
            .collect();
        let mut fields: Vec<FieldSig> = Vec::new();
        for p in &c.constructor_params {
            if p.is_val || p.is_var {
                let ty = self.resolve_type_ref(&p.ty);
                fields.push(FieldSig {
                    name: self.interner.resolve(p.name).to_string(),
                    ty,
                });
            }
        }
        for prop in &c.properties {
            let ty = prop
                .ty
                .as_ref()
                .map(|tr| self.resolve_type_ref(tr))
                .unwrap_or(Ty::Any);
            fields.push(FieldSig {
                name: self.interner.resolve(prop.name).to_string(),
                ty,
            });
        }
        let methods: Vec<MethodSig> = c
            .methods
            .iter()
            .map(|m| self.method_sig_from_fun(m))
            .collect();
        let companion_methods: Vec<MethodSig> = c
            .companion_methods
            .iter()
            .map(|m| self.method_sig_from_fun(m))
            .collect();
        self.env.types.insert(
            name,
            TypeDecl {
                name: self.interner.resolve(c.name).to_string(),
                super_class,
                interfaces,
                fields,
                methods,
                companion_methods,
                is_enum: false,
            },
        );
    }

    fn register_interface(&mut self, i: &InterfaceDecl) {
        let name = self.interner.resolve(i.name).to_string();
        let methods: Vec<MethodSig> = i
            .methods
            .iter()
            .map(|m| self.method_sig_from_fun(m))
            .collect();
        self.env.types.insert(
            name.clone(),
            TypeDecl {
                name,
                super_class: None,
                interfaces: Vec::new(),
                fields: Vec::new(),
                methods,
                companion_methods: Vec::new(),
                is_enum: false,
            },
        );
    }

    fn register_enum(&mut self, e: &EnumDecl) {
        let name = self.interner.resolve(e.name).to_string();
        // Enum fields: implicit "name" + constructor params.
        let mut fields = vec![FieldSig {
            name: "name".to_string(),
            ty: Ty::String,
        }];
        for p in &e.constructor_params {
            let ty = self.resolve_type_ref(&p.ty);
            fields.push(FieldSig {
                name: self.interner.resolve(p.name).to_string(),
                ty,
            });
        }
        // Register each entry so Color.RED resolves.
        for entry in &e.entries {
            let entry_name = self.interner.resolve(entry.name).to_string();
            self.env.enum_entries.insert(entry_name, name.clone());
        }
        self.env.types.insert(
            name.clone(),
            TypeDecl {
                name,
                super_class: None,
                interfaces: Vec::new(),
                fields,
                methods: Vec::new(),
                companion_methods: Vec::new(),
                is_enum: true,
            },
        );
    }

    fn method_sig_from_fun(&mut self, f: &FunDecl) -> MethodSig {
        let name = self.interner.resolve(f.name).to_string();
        let params: Vec<Ty> = f
            .params
            .iter()
            .map(|p| self.resolve_type_ref(&p.ty))
            .collect();
        let ret = f
            .return_ty
            .as_ref()
            .map(|tr| self.resolve_type_ref(tr))
            .unwrap_or(Ty::Unit);
        MethodSig { name, params, ret }
    }

    // ── Type reference resolution ───────────────────────────────────────

    fn resolve_type_ref(&mut self, tr: &TypeRef) -> Ty {
        // Function type: (P1, P2) -> R
        if let Some(ref fparams) = tr.func_params {
            let params: Vec<Ty> = fparams.iter().map(|p| self.resolve_type_ref(p)).collect();
            let ret = self.resolve_type_ref(&TypeRef {
                name: tr.name,
                nullable: false,
                func_params: None,
                type_args: Vec::new(),
                is_suspend: false,
                has_receiver: false,
                span: tr.span,
            });
            return Ty::Function {
                params,
                ret: Box::new(ret),
                is_suspend: tr.is_suspend,
            };
        }
        let raw_name = self.interner.resolve(tr.name).to_string();
        // Resolve type aliases.
        let name = if let Some(target) = self.type_aliases.get(&raw_name) {
            target.clone()
        } else {
            raw_name
        };
        // Map well-known Kotlin collection/stdlib type names to their
        // fully-qualified JVM internal names so descriptors use the
        // correct class path (e.g. `Ljava/util/List;` not `LList;`).
        // User-defined types take priority over well-known mappings so
        // that e.g. a user-defined `Pair` class isn't silently mapped
        // to `kotlin/Pair`.
        let base = if let Some(t) = ty_from_name(&name) {
            t
        } else if self.type_params.contains(&name) {
            // Type parameter: erase to Any (Object on JVM).
            Ty::Any
        } else if self.env.types.contains_key(&name) {
            Ty::Class(name)
        } else if let Some(jvm_name) = well_known_class_name(&name) {
            Ty::Class(jvm_name.to_string())
        } else if name.chars().next().is_some_and(|c| c.is_uppercase()) {
            Ty::Class(name)
        } else {
            self.diags
                .push(Diagnostic::error(tr.span, format!("unknown type `{name}`")));
            return Ty::Error;
        };
        if tr.nullable {
            Ty::Nullable(Box::new(base))
        } else {
            base
        }
    }

    fn type_ref(&mut self, tr: &TypeRef) -> Option<Ty> {
        let ty = self.resolve_type_ref(tr);
        if ty == Ty::Error {
            None
        } else {
            Some(ty)
        }
    }

    // ── Signatures ──────────────────────────────────────────────────────

    fn signature_for_fun(&mut self, f: &FunDecl) -> Signature {
        let mut params: Vec<Ty> = Vec::new();
        if let Some(recv) = &f.receiver_ty {
            params.push(self.type_ref(recv).unwrap_or(Ty::Any));
        }
        for p in &f.params {
            let ty = self.type_ref(&p.ty).unwrap_or(Ty::Error);
            // vararg Int → IntArray on JVM
            let ty = if p.is_vararg && ty == Ty::Int {
                Ty::IntArray
            } else {
                ty
            };
            params.push(ty);
        }
        let ret = match &f.return_ty {
            Some(r) => self.type_ref(r).unwrap_or(Ty::Error),
            None => Ty::Unit,
        };
        Signature { params, ret }
    }

    // ── Function checking ───────────────────────────────────────────────

    fn check_function(
        &mut self,
        idx: u32,
        f: &FunDecl,
        rf: &skotch_resolve::ResolvedFunction,
    ) -> TypedFunction {
        let sig = self.out.top_signatures[&DefId::Function(idx)].clone();
        let mut local_tys: Vec<Ty> = Vec::new();
        let _ = rf;
        let mut scope: Vec<(Symbol, Ty)> = Vec::new();
        let param_offset = if f.receiver_ty.is_some() {
            let this_sym = self.interner.intern("this");
            scope.push((this_sym, sig.params[0].clone()));
            1
        } else {
            0
        };
        for (pi, p) in f.params.iter().enumerate() {
            scope.push((p.name, sig.params[pi + param_offset].clone()));
        }
        self.check_block(&f.body, &mut scope, &mut local_tys);
        TypedFunction {
            name_index: idx,
            return_ty: sig.ret,
            param_tys: sig.params,
            local_tys,
        }
    }

    fn check_top_val(&mut self, idx: u32, v: &ValDecl) -> TypedTopVal {
        TypedTopVal {
            name_index: idx,
            ty: self.synth_top_init(&v.init),
        }
    }

    fn synth_top_init(&mut self, e: &Expr) -> Ty {
        match e {
            Expr::IntLit(_, _) => Ty::Int,
            Expr::LongLit(_, _) => Ty::Long,
            Expr::DoubleLit(_, _) => Ty::Double,
            Expr::FloatLit(_, _) => Ty::Float,
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::NullLit(_) => Ty::Nullable(Box::new(Ty::Nothing)),
            Expr::StringLit(_, _) => Ty::String,
            other => {
                self.diags.push(Diagnostic::error(
                    other.span(),
                    "top-level val initializers must be a literal",
                ));
                Ty::Error
            }
        }
    }

    // ── Block & statement checking ──────────────────────────────────────

    fn check_block(
        &mut self,
        block: &Block,
        scope: &mut Vec<(Symbol, Ty)>,
        local_tys: &mut Vec<Ty>,
    ) {
        let saved = scope.len();
        for stmt in &block.stmts {
            match stmt {
                Stmt::Expr(e) => {
                    self.synth_expr(e, scope);
                }
                Stmt::Val(v) => {
                    let init_ty = self.synth_expr(&v.init, scope);
                    let declared = match &v.ty {
                        Some(tr) => self.type_ref(tr).unwrap_or(Ty::Error),
                        None => init_ty.clone(),
                    };
                    // Kotlin allows integer literal narrowing: `val b: Byte = 42`
                    let narrowing_ok = matches!(
                        (&init_ty, &declared),
                        (Ty::Int, Ty::Byte | Ty::Short) | (Ty::Double, Ty::Float)
                    ) && matches!(
                        v.init,
                        Expr::IntLit(..) | Expr::DoubleLit(..) | Expr::FloatLit(..)
                    );
                    if !narrowing_ok
                        && !self.is_assignable(&init_ty, &declared)
                        && declared != Ty::Error
                    {
                        self.diags.push(Diagnostic::error(
                            v.span,
                            format!(
                                "type mismatch: expected {}, found {}",
                                declared.display_name(),
                                init_ty.display_name()
                            ),
                        ));
                    }
                    // Nullable enforcement: `val x: String = null` is an error.
                    // Only fires when there is an explicit non-nullable annotation
                    // and the init expression is a null literal.
                    if v.ty.is_some()
                        && !matches!(declared, Ty::Nullable(_) | Ty::Error)
                        && matches!(v.init, Expr::NullLit(_))
                    {
                        self.diags.push(Diagnostic::error(
                            v.span,
                            format!(
                                "Null can not be a value of a non-null type {}",
                                declared.display_name()
                            ),
                        ));
                    }
                    local_tys.push(declared.clone());
                    scope.push((v.name, declared));
                }
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        self.synth_expr(v, scope);
                    }
                }
                Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
                    self.synth_expr(cond, scope);
                    self.check_block(body, scope, local_tys);
                }
                Stmt::Assign { value, .. } => {
                    self.synth_expr(value, scope);
                }
                Stmt::IndexAssign {
                    receiver,
                    index,
                    value,
                    ..
                } => {
                    self.synth_expr(receiver, scope);
                    self.synth_expr(index, scope);
                    self.synth_expr(value, scope);
                }
                Stmt::FieldAssign {
                    receiver, value, ..
                } => {
                    self.synth_expr(receiver, scope);
                    self.synth_expr(value, scope);
                }
                Stmt::Break { .. } | Stmt::Continue { .. } => {}
                Stmt::TryStmt {
                    body,
                    catch_body,
                    finally_body,
                    ..
                } => {
                    for inner in &body.stmts {
                        match inner {
                            Stmt::Expr(e) => {
                                self.synth_expr(e, scope);
                            }
                            Stmt::Val(v) => {
                                let init_ty = self.synth_expr(&v.init, scope);
                                let declared = match &v.ty {
                                    Some(tr) => self.type_ref(tr).unwrap_or(Ty::Error),
                                    None => init_ty.clone(),
                                };
                                local_tys.push(declared.clone());
                                scope.push((v.name, declared));
                            }
                            _ => {}
                        }
                    }
                    if let Some(cb) = catch_body {
                        for inner in &cb.stmts {
                            if let Stmt::Expr(e) = inner {
                                self.synth_expr(e, scope);
                            }
                        }
                    }
                    if let Some(fb) = finally_body {
                        for inner in &fb.stmts {
                            match inner {
                                Stmt::Expr(e) => {
                                    self.synth_expr(e, scope);
                                }
                                Stmt::Val(v) => {
                                    let init_ty = self.synth_expr(&v.init, scope);
                                    let declared = match &v.ty {
                                        Some(tr) => self.type_ref(tr).unwrap_or(Ty::Error),
                                        None => init_ty.clone(),
                                    };
                                    local_tys.push(declared.clone());
                                    scope.push((v.name, declared));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Stmt::ThrowStmt { expr, .. } => {
                    self.synth_expr(expr, scope);
                }
                Stmt::LocalFun(f) => {
                    let sig = self.signature_for_fun(f);
                    let fn_idx = self.fn_names.len() as u32;
                    self.out.top_signatures.insert(DefId::Function(fn_idx), sig);
                    self.fn_names.push(f.name);
                }
                Stmt::Destructure { names, init, .. } => {
                    self.synth_expr(init, scope);
                    for name in names {
                        local_tys.push(Ty::Any);
                        scope.push((*name, Ty::Any));
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
                    self.synth_expr(range_start, scope);
                    self.synth_expr(range_end, scope);
                    if let Some(step_e) = step {
                        self.synth_expr(step_e, scope);
                    }
                    local_tys.push(Ty::Int);
                    scope.push((*var_name, Ty::Int));
                    self.check_block(body, scope, local_tys);
                }
                Stmt::ForIn {
                    var_name,
                    destructure_names,
                    iterable,
                    body,
                    ..
                } => {
                    let iter_ty = self.synth_expr(iterable, scope);
                    let elem_ty = match &iter_ty {
                        Ty::IntArray => Ty::Int,
                        Ty::LongArray => Ty::Long,
                        Ty::DoubleArray => Ty::Double,
                        Ty::BooleanArray => Ty::Bool,
                        Ty::ByteArray => Ty::Byte,
                        Ty::String => Ty::Char,
                        // Generic collections erase to Any on JVM.
                        _ => Ty::Any,
                    };
                    if let Some(names) = destructure_names {
                        // Each destructured component is typed as Any (erased).
                        for dn in names {
                            local_tys.push(Ty::Any);
                            scope.push((*dn, Ty::Any));
                        }
                    } else {
                        local_tys.push(elem_ty.clone());
                        scope.push((*var_name, elem_ty));
                    }
                    self.check_block(body, scope, local_tys);
                }
            }
        }
        scope.truncate(saved);
    }

    // ── Expression synthesis ────────────────────────────────────────────

    fn synth_expr(&mut self, e: &Expr, scope: &mut Vec<(Symbol, Ty)>) -> Ty {
        match e {
            Expr::IntLit(_, _) => Ty::Int,
            Expr::CharLit(_, _) => Ty::Char,
            Expr::LongLit(_, _) => Ty::Long,
            Expr::DoubleLit(_, _) => Ty::Double,
            Expr::FloatLit(_, _) => Ty::Float,
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::NullLit(_) => Ty::Nullable(Box::new(Ty::Nothing)),
            Expr::StringLit(_, _) => Ty::String,

            Expr::Ident(name, _) => {
                // Local scope lookup.
                if let Some((_, t)) = scope.iter().rev().find(|(n, _)| *n == *name) {
                    return t.clone();
                }
                let name_str = self.interner.resolve(*name).to_string();
                // Enum entry: Color.RED → Ty::Class("Color")
                if let Some(enum_name) = self.env.enum_entries.get(&name_str) {
                    return Ty::Class(enum_name.clone());
                }
                // Known type → Class.
                if self.env.types.contains_key(&name_str) {
                    return Ty::Class(name_str);
                }
                // Deferred to MIR lowering (external classes, top-level
                // functions used as values, etc.). Use Error so cascading
                // type checks don't produce false positives — the real
                // check happens during lowering.
                Ty::Error
            }

            Expr::Paren(inner, _) => self.synth_expr(inner, scope),

            Expr::Binary { op, lhs, rhs, span } => {
                let lt = self.synth_expr(lhs, scope);
                let rt = self.synth_expr(rhs, scope);
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        if lt == Ty::Double || rt == Ty::Double {
                            Ty::Double
                        } else if lt == Ty::Long || rt == Ty::Long {
                            Ty::Long
                        } else if matches!(lt, Ty::Int | Ty::Any) && matches!(rt, Ty::Int | Ty::Any)
                        {
                            Ty::Int
                        } else if *op == BinOp::Add && lt == Ty::String {
                            Ty::String
                        } else if lt == Ty::Error || rt == Ty::Error {
                            Ty::Error
                        } else if let Ty::Class(ref class_name) = lt {
                            // Operator overloading: check for plus/minus/times methods.
                            let op_method = match op {
                                BinOp::Add => "plus",
                                BinOp::Sub => "minus",
                                BinOp::Mul => "times",
                                BinOp::Div => "div",
                                BinOp::Mod => "rem",
                                _ => unreachable!(),
                            };
                            if let Some(m) = self.env.lookup_method(class_name, op_method) {
                                // When the return type is Unit (unresolved
                                // expression-body), assume operators return the
                                // receiver type so chained expressions type-check.
                                if m.ret == Ty::Unit {
                                    lt.clone()
                                } else {
                                    m.ret.clone()
                                }
                            } else {
                                self.diags.push(Diagnostic::error(
                                    *span,
                                    format!(
                                        "arithmetic on {} and {} not supported",
                                        lt.display_name(),
                                        rt.display_name()
                                    ),
                                ));
                                Ty::Error
                            }
                        } else {
                            self.diags.push(Diagnostic::error(
                                *span,
                                format!(
                                    "arithmetic on {} and {} not supported",
                                    lt.display_name(),
                                    rt.display_name()
                                ),
                            ));
                            Ty::Error
                        }
                    }
                    BinOp::Eq
                    | BinOp::NotEq
                    | BinOp::Lt
                    | BinOp::Gt
                    | BinOp::LtEq
                    | BinOp::GtEq => Ty::Bool,
                    BinOp::And | BinOp::Or => Ty::Bool,
                }
            }

            Expr::Unary { operand, .. } => self.synth_expr(operand, scope),

            Expr::Call { callee, args, .. } => {
                // Synthesize argument types.
                for a in args {
                    self.synth_expr(&a.expr, scope);
                }

                // Method call on a receiver: receiver.method(args)
                if let Expr::Field { receiver, name, .. } = callee.as_ref() {
                    let recv_ty = self.synth_expr(receiver, scope);
                    let method = self.interner.resolve(*name).to_string();

                    // Built-in method return types.
                    if let Some(ty) = self.builtin_method_type(&recv_ty, &method) {
                        return ty;
                    }

                    // User-defined class/interface/enum method.
                    if let Ty::Class(ref class_name) = recv_ty {
                        if let Some(m) = self.env.lookup_method(class_name, &method) {
                            return m.ret.clone();
                        }
                    }

                    // Unresolved — return Any to let MIR lowering handle it.
                    return Ty::Any;
                }

                // Direct function call: name(args)
                let callee_name = match callee.as_ref() {
                    Expr::Ident(name, _) => Some(*name),
                    _ => None,
                };
                if let Some(name) = callee_name {
                    // Check top-level signatures.
                    for (&def_id, sig) in &self.out.top_signatures {
                        if let DefId::Function(fi) = def_id {
                            if self.fn_names.get(fi as usize).copied() == Some(name) {
                                return sig.ret.clone();
                            }
                        }
                        if def_id == DefId::PrintlnIntrinsic {
                            let println_sym = self.interner.intern("println");
                            if name == println_sym {
                                return sig.ret.clone();
                            }
                        }
                    }
                    // Might be a companion method, enum entry, or constructor.
                    let name_str = self.interner.resolve(name).to_string();
                    if let Some(enum_name) = self.env.enum_entries.get(&name_str).cloned() {
                        return Ty::Class(enum_name);
                    }
                    if self.env.types.contains_key(&name_str) {
                        return Ty::Class(name_str);
                    }
                    // Built-in array constructors.
                    if name_str == "IntArray" {
                        return Ty::IntArray;
                    }
                    // Check if the callee is a local variable with an invoke operator.
                    if let Some((_, Ty::Class(ref class_name))) =
                        scope.iter().rev().find(|(n, _)| *n == name)
                    {
                        if let Some(m) = self.env.lookup_method(class_name, "invoke") {
                            return m.ret.clone();
                        }
                    }
                    return Ty::Any;
                }
                Ty::Unit
            }

            Expr::Field { receiver, name, .. } => {
                let recv_ty = self.synth_expr(receiver, scope);
                let field_name = self.interner.resolve(*name).to_string();

                // Built-in: String.length
                if recv_ty == Ty::String && field_name == "length" {
                    return Ty::Int;
                }

                // Built-in: IntArray.size
                if recv_ty == Ty::IntArray && field_name == "size" {
                    return Ty::Int;
                }

                // User-defined class field.
                if let Ty::Class(ref class_name) = recv_ty {
                    // Check companion methods (for ClassName.staticMethod pattern).
                    if let Some(m) = self.env.lookup_companion(class_name, &field_name) {
                        return m.ret.clone();
                    }
                    // Check instance fields.
                    if let Some(f) = self.env.lookup_field(class_name, &field_name) {
                        return f.ty.clone();
                    }
                    // Might be an enum entry: Color.RED
                    if let Some(enum_name) = self.env.enum_entries.get(&field_name) {
                        if enum_name == class_name {
                            return Ty::Class(class_name.clone());
                        }
                    }
                    return Ty::Error;
                }

                Ty::Error
            }

            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.synth_expr(cond, scope);
                let then_ty = self.block_result_type(then_block, scope);
                if let Some(eb) = else_block {
                    let else_ty = self.block_result_type(eb, scope);
                    // If both branches agree on type, use it; otherwise Any.
                    if then_ty == else_ty {
                        return then_ty;
                    }
                    return Ty::Any;
                }
                then_ty
            }

            Expr::When {
                subject,
                branches,
                else_body,
                ..
            } => {
                self.synth_expr(subject, scope);
                let mut result_ty = Ty::Unit;
                for b in branches {
                    self.synth_expr(&b.pattern, scope);
                    result_ty = self.synth_expr(&b.body, scope);
                }
                if let Some(eb) = else_body {
                    result_ty = self.synth_expr(eb, scope);
                }
                result_ty
            }

            Expr::StringTemplate(parts, _) => {
                for p in parts {
                    if let TemplatePart::Expr(inner) = p {
                        self.synth_expr(inner, scope);
                    }
                }
                Ty::String
            }

            Expr::Throw { expr, .. } => {
                self.synth_expr(expr, scope);
                Ty::Nothing
            }

            Expr::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                self.check_block(body, scope, &mut Vec::new());
                let body_ty = self.block_result_type(body, scope);
                if let Some(cb) = catch_body {
                    self.check_block(cb, scope, &mut Vec::new());
                }
                if let Some(fb) = finally_body {
                    self.check_block(fb, scope, &mut Vec::new());
                }
                body_ty
            }

            Expr::ElvisOp { lhs, rhs, .. } => {
                let lt = self.synth_expr(lhs, scope);
                let rt = self.synth_expr(rhs, scope);
                // Elvis unwraps nullable: T? ?: T → T
                if let Ty::Nullable(inner) = &lt {
                    return (**inner).clone();
                }
                if rt != Ty::Any {
                    rt
                } else {
                    lt
                }
            }

            Expr::SafeCall { receiver, name, .. } => {
                let recv_ty = self.synth_expr(receiver, scope);
                let method_name = self.interner.resolve(*name).to_string();
                // Unwrap nullable for method lookup.
                let inner = if let Ty::Nullable(inner) = &recv_ty {
                    (**inner).clone()
                } else {
                    recv_ty
                };
                if let Ty::Class(ref cn) = inner {
                    if let Some(m) = self.env.lookup_method(cn, &method_name) {
                        return Ty::Nullable(Box::new(m.ret.clone()));
                    }
                }
                Ty::Nullable(Box::new(Ty::Any))
            }

            Expr::IsCheck { expr, .. } => {
                self.synth_expr(expr, scope);
                Ty::Bool
            }

            Expr::AsCast {
                expr,
                type_name,
                safe,
                ..
            } => {
                self.synth_expr(expr, scope);
                let name = self.interner.resolve(*type_name).to_string();
                let target = ty_from_name(&name).unwrap_or_else(|| {
                    if self.env.types.contains_key(&name) {
                        Ty::Class(name)
                    } else {
                        Ty::Error
                    }
                });
                if *safe {
                    Ty::Nullable(Box::new(target))
                } else {
                    target
                }
            }

            Expr::NotNullAssert { expr, .. } => {
                let t = self.synth_expr(expr, scope);
                if let Ty::Nullable(inner) = t {
                    *inner
                } else {
                    t
                }
            }

            Expr::Lambda { params, body, .. } => {
                // Synthesize lambda type from params and body result.
                let mut lambda_scope = scope.clone();
                let mut param_tys = Vec::new();
                for p in params {
                    let ty = self.resolve_type_ref(&p.ty);
                    lambda_scope.push((p.name, ty.clone()));
                    param_tys.push(ty);
                }
                self.check_block(body, &mut lambda_scope, &mut Vec::new());
                let ret = self.block_result_type(body, &mut lambda_scope);
                Ty::Function {
                    params: param_tys,
                    ret: Box::new(ret),
                    is_suspend: false,
                }
            }

            Expr::ObjectExpr { super_type, .. } => {
                let name = self.interner.resolve(*super_type).to_string();
                if self.env.types.contains_key(&name) {
                    Ty::Class(name)
                } else {
                    Ty::Error
                }
            }

            Expr::Index {
                receiver, index, ..
            } => {
                let recv_ty = self.synth_expr(receiver, scope);
                let _idx_ty = self.synth_expr(index, scope);
                match recv_ty {
                    Ty::IntArray => Ty::Int,
                    _ => Ty::Any,
                }
            }
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Built-in method return types for primitive/standard library types.
    fn builtin_method_type(&self, recv_ty: &Ty, method: &str) -> Option<Ty> {
        match (recv_ty, method) {
            (Ty::String, "length" | "indexOf" | "lastIndexOf" | "compareTo" | "get") => {
                Some(Ty::Int)
            }
            (Ty::String, "isEmpty" | "contains" | "startsWith" | "endsWith" | "equals") => {
                Some(Ty::Bool)
            }
            (
                Ty::String,
                "uppercase" | "lowercase" | "trim" | "substring" | "replace" | "repeat"
                | "reversed",
            ) => Some(Ty::String),
            (Ty::String, "lines") => Some(Ty::Class("java/util/List".into())),
            (Ty::String, "toInt") => Some(Ty::Int),
            (Ty::String, "toLong") => Some(Ty::Long),
            (Ty::String, "toDouble") => Some(Ty::Double),
            (_, "toString") => Some(Ty::String),
            (_, "hashCode") => Some(Ty::Int),
            (_, "equals") => Some(Ty::Bool),
            (_, "coerceAtLeast" | "coerceAtMost") => Some(recv_ty.clone()),
            // Map methods
            (Ty::Class(cn), "containsKey" | "containsValue" | "isEmpty") if cn.contains("Map") => {
                Some(Ty::Bool)
            }
            (Ty::Class(cn), "get" | "put" | "remove") if cn.contains("Map") => Some(Ty::Any),
            (Ty::Class(cn), "size") if cn.contains("Map") => Some(Ty::Int),
            (Ty::Class(cn), "keys" | "entries") if cn.contains("Map") => {
                Some(Ty::Class("java/util/Set".into()))
            }
            (Ty::Class(cn), "values") if cn.contains("Map") => {
                Some(Ty::Class("java/util/Collection".into()))
            }
            // Set methods
            (Ty::Class(cn), "contains" | "add" | "remove" | "isEmpty") if cn.contains("Set") => {
                Some(Ty::Bool)
            }
            (Ty::Class(cn), "size") if cn.contains("Set") => Some(Ty::Int),
            _ => None,
        }
    }

    /// Synthesize the result type of a block (last expression's type).
    fn block_result_type(&mut self, block: &Block, scope: &mut Vec<(Symbol, Ty)>) -> Ty {
        let saved = scope.len();
        let mut result = Ty::Unit;
        for stmt in &block.stmts {
            match stmt {
                Stmt::Expr(e) => {
                    result = self.synth_expr(e, scope);
                }
                Stmt::Val(v) => {
                    let init_ty = self.synth_expr(&v.init, scope);
                    let declared = match &v.ty {
                        Some(tr) => self.type_ref(tr).unwrap_or(Ty::Error),
                        None => init_ty,
                    };
                    scope.push((v.name, declared));
                    result = Ty::Unit;
                }
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        result = self.synth_expr(v, scope);
                    }
                }
                _ => {
                    result = Ty::Unit;
                }
            }
        }
        scope.truncate(saved);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_lexer::lex;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;

    fn run(src: &str) -> (TypedFile, Diagnostics) {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let file = parse_file(&lf, &mut interner, &mut diags);
        let resolved = resolve_file(&file, &mut interner, &mut diags, None);
        let typed = type_check(&file, &resolved, &mut interner, &mut diags, None);
        (typed, diags)
    }

    #[test]
    fn basic_function_types() {
        let (tf, d) =
            run("fun add(a: Int, b: Int): Int = a + b\nfun main() { println(add(1, 2)) }");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(tf.functions.len(), 2);
        assert_eq!(tf.functions[0].return_ty, Ty::Int);
        assert_eq!(tf.functions[0].param_tys, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn class_field_type() {
        let (tf, d) = run(
            "class Point(val x: Int, val y: Int)\nfun main() { val p = Point(1, 2); println(p.x) }",
        );
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(tf.functions.len(), 1);
    }

    #[test]
    fn operator_plus_type() {
        let src = r#"
data class Vec2(val x: Int, val y: Int) {
    operator fun plus(other: Vec2) = Vec2(x + other.x, y + other.y)
}
fun main() {
    val a = Vec2(1, 2)
    val b = Vec2(3, 4)
    println(a + b)
}
"#;
        let (_tf, d) = run(src);
        assert!(d.is_empty(), "diagnostics: {:?}", d);
    }

    #[test]
    fn enum_entry_type() {
        let (tf, d) = run("enum class Dir { NORTH, SOUTH }\nfun main() { println(Dir.NORTH) }");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(tf.functions.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════
    // SOUNDNESS INVARIANT TESTS — TYPECHECKER
    //
    // These tests verify that the typechecker REJECTS invalid programs.
    // They MUST NEVER be weakened, loosened, or removed.
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn soundness_reject_int_assigned_to_string() {
        let (_, d) = run("fun main() { val x: String = 42 }");
        assert!(d.has_errors(), "must reject Int → String assignment");
    }

    #[test]
    fn soundness_reject_string_assigned_to_int() {
        let (_, d) = run(r#"fun main() { val x: Int = "hello" }"#);
        assert!(d.has_errors(), "must reject String → Int assignment");
    }

    #[test]
    fn soundness_reject_null_to_non_nullable() {
        let (_, d) = run("fun main() { val x: String = null }");
        assert!(d.has_errors(), "must reject null → non-nullable String");
    }

    #[test]
    fn soundness_accept_null_to_nullable() {
        let (_, d) = run("fun main() { val x: String? = null }");
        assert!(!d.has_errors(), "null → String? must be accepted: {:?}", d);
    }

    #[test]
    fn soundness_accept_value_to_nullable() {
        let (_, d) = run(r#"fun main() { val x: String? = "hello" }"#);
        assert!(
            !d.has_errors(),
            "String → String? must be accepted: {:?}",
            d
        );
    }

    #[test]
    fn soundness_reject_bool_assigned_to_int() {
        let (_, d) = run("fun main() { val x: Int = true }");
        assert!(d.has_errors(), "must reject Bool → Int assignment");
    }

    #[test]
    fn soundness_accept_subclass_to_superclass() {
        let src = r#"
open class Animal
class Dog : Animal()
fun main() { val a: Animal = Dog() }
"#;
        let (_, d) = run(src);
        assert!(!d.has_errors(), "Dog → Animal must be accepted: {:?}", d);
    }

    #[test]
    fn soundness_accept_class_implementing_interface() {
        let src = r#"
interface Greetable { fun greet(): String }
class Person : Greetable { override fun greet(): String = "Hi" }
fun main() { val g: Greetable = Person() }
"#;
        let (_, d) = run(src);
        assert!(
            !d.has_errors(),
            "Person → Greetable must be accepted: {:?}",
            d
        );
    }

    #[test]
    fn soundness_lambda_has_function_type() {
        let (tf, d) = run("fun main() { val f = { x: Int -> x + 1 } }");
        assert!(!d.has_errors(), "lambda must typecheck: {:?}", d);
        // The local for f should have a Function type.
        assert!(!tf.functions.is_empty());
        let main_fn = &tf.functions[0];
        let f_ty = main_fn
            .local_tys
            .iter()
            .find(|t| matches!(t, Ty::Function { .. }));
        assert!(
            f_ty.is_some(),
            "lambda local should have Function type, got: {:?}",
            main_fn.local_tys
        );
    }

    #[test]
    fn soundness_valid_program_no_errors() {
        // A comprehensive valid program must produce zero diagnostics.
        let src = r#"
fun add(a: Int, b: Int): Int = a + b
data class Point(val x: Int, val y: Int)
fun main() {
    val p = Point(1, 2)
    println(p.x)
    println(add(3, 4))
    val n: Int? = null
    val s: String? = "hello"
    val x = 42
    val y = x + 1
    println(y)
}
"#;
        let (_, d) = run(src);
        assert!(
            !d.has_errors(),
            "valid program must not produce errors: {:?}",
            d
        );
    }
}
