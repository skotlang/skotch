//! Type checker for the Kotlin subset skotch accepts.
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
}

// ─── Entry point ────────────────────────────────────────────────────────────

pub fn type_check(
    file: &KtFile,
    resolved: &ResolvedFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> TypedFile {
    let mut tc = TypeChecker {
        interner,
        diags,
        out: TypedFile::default(),
        fn_names: Vec::new(),
        env: TypeEnv::default(),
        type_params: Vec::new(),
    };

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
            Decl::Class(c) => {
                let class_idx = 10000 + tc.fn_names.len() as u32;
                let name = tc.interner.resolve(c.name).to_string();
                let sig = Signature {
                    params: vec![],
                    ret: Ty::Class(name),
                };
                tc.out
                    .top_signatures
                    .insert(DefId::Function(class_idx), sig);
                tc.fn_names.push(c.name);
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
}

impl<'a> TypeChecker<'a> {
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
                span: tr.span,
            });
            return Ty::Function {
                params,
                ret: Box::new(ret),
            };
        }
        let name = self.interner.resolve(tr.name).to_string();
        let base = if let Some(t) = ty_from_name(&name) {
            t
        } else if self.type_params.contains(&name) {
            // Type parameter: erase to Any (Object on JVM).
            Ty::Any
        } else if self.env.types.contains_key(&name)
            || name.chars().next().is_some_and(|c| c.is_uppercase())
        {
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
            params.push(self.type_ref(&p.ty).unwrap_or(Ty::Error));
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
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::NullLit(_) => Ty::Nullable(Box::new(Ty::Any)),
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
                    if !init_ty.assignable_to(&declared) && declared != Ty::Error {
                        self.diags.push(Diagnostic::error(
                            v.span,
                            format!(
                                "type mismatch: expected {}, found {}",
                                declared.display_name(),
                                init_ty.display_name()
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
                Stmt::Break(_) | Stmt::Continue(_) => {}
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
                Stmt::For {
                    var_name,
                    start: range_start,
                    end: range_end,
                    body,
                    ..
                } => {
                    self.synth_expr(range_start, scope);
                    self.synth_expr(range_end, scope);
                    local_tys.push(Ty::Int);
                    scope.push((*var_name, Ty::Int));
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
            Expr::LongLit(_, _) => Ty::Long,
            Expr::DoubleLit(_, _) => Ty::Double,
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::NullLit(_) => Ty::Nullable(Box::new(Ty::Any)),
            Expr::StringLit(_, _) => Ty::String,

            Expr::Ident(name, _) => {
                // Local scope lookup.
                if let Some((_, t)) = scope.iter().rev().find(|(n, _)| *n == *name) {
                    return t.clone();
                }
                // Top-level val/function.
                let name_str = self.interner.resolve(*name).to_string();
                // Enum entry: Color.RED → Ty::Class("Color")
                if let Some(enum_name) = self.env.enum_entries.get(&name_str) {
                    return Ty::Class(enum_name.clone());
                }
                Ty::Any
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
                    return Ty::Any;
                }

                Ty::Any
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
                Ty::Unit
            }

            Expr::Try {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                self.check_block(body, scope, &mut Vec::new());
                if let Some(cb) = catch_body {
                    self.check_block(cb, scope, &mut Vec::new());
                }
                if let Some(fb) = finally_body {
                    self.check_block(fb, scope, &mut Vec::new());
                }
                Ty::Unit
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
                        Ty::Any
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
                for p in params {
                    let ty = self.resolve_type_ref(&p.ty);
                    lambda_scope.push((p.name, ty));
                }
                let ret = self.block_result_type(body, &mut lambda_scope);
                // Return as a class type with synthetic name — the MIR lowering
                // generates the actual class. The typechecker just needs to
                // propagate this so `val f = { x: Int -> x }; f(5)` resolves.
                let _ = ret;
                Ty::Any // Lambda type details resolved by MIR lowering
            }

            Expr::ObjectExpr { super_type, .. } => {
                let name = self.interner.resolve(*super_type).to_string();
                if self.env.types.contains_key(&name) {
                    Ty::Class(name)
                } else {
                    Ty::Any
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
                "uppercase" | "lowercase" | "trim" | "substring" | "replace" | "repeat",
            ) => Some(Ty::String),
            (Ty::String, "toInt") => Some(Ty::Int),
            (Ty::String, "toLong") => Some(Ty::Long),
            (Ty::String, "toDouble") => Some(Ty::Double),
            (Ty::Int | Ty::Long | Ty::Double, "toString") => Some(Ty::String),
            (_, "coerceAtLeast" | "coerceAtMost") => Some(recv_ty.clone()),
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
        let resolved = resolve_file(&file, &mut interner, &mut diags);
        let typed = type_check(&file, &resolved, &mut interner, &mut diags);
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
    fn enum_entry_type() {
        let (tf, d) = run("enum class Dir { NORTH, SOUTH }\nfun main() { println(Dir.NORTH) }");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(tf.functions.len(), 1);
    }
}
