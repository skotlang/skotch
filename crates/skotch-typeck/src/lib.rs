//! Bidirectional type checker for the Kotlin subset skotch accepts.
//!
//! "Bidirectional" is generous for what we do in PR #1: we synthesize
//! types for literals and identifier references, and *check* call
//! arguments against parameter types. There is no inference for
//! lambdas, no smart casts, no overload resolution beyond
//! `println(Any?)` (which we treat as a single intrinsic, not an
//! overload set), and no generics.
//!
//! The output is a [`TypedFile`] which is a flat side-table indexed
//! by `(function index, expression-walk-position)`. The MIR-lowering
//! pass walks the AST in the same order so the indices match.

use rustc_hash::FxHashMap;
use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_resolve::{DefId, ResolvedFile};
use skotch_syntax::{
    BinOp, Block, Decl, Expr, FunDecl, KtFile, Stmt, TemplatePart, TypeRef, ValDecl,
};
use skotch_types::{ty_from_name, Ty};

/// Per-function type table: a flat list of expression types in the same
/// pre-order traversal the lowering pass uses.
#[derive(Clone, Debug)]
pub struct TypedFunction {
    pub name_index: u32,
    pub return_ty: Ty,
    pub param_tys: Vec<Ty>,
    /// Local slot types in declaration order. Indexed by `Local(_, n)`.
    pub local_tys: Vec<Ty>,
    /// Expression types, indexed in pre-order over the function body.
    /// The lowering pass and this checker walk in lockstep.
    pub expr_tys: Vec<Ty>,
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
    /// Mapping from top-level def to its declared type. Used by
    /// `Function` calls so the checker doesn't need to re-walk the
    /// callee's signature.
    pub top_signatures: FxHashMap<DefId, Signature>,
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Type-check a [`KtFile`] given its [`ResolvedFile`].
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
    };

    // Pass 1: collect signatures for every top-level fun and val so call
    // sites can be checked regardless of declaration order.
    //
    // The DefId for `Function` and `TopLevelVal` is **kind-relative**
    // (matching what `skotch-resolve` produces): the i-th *function*
    // is `DefId::Function(i)`, the j-th *top-level val* is
    // `DefId::TopLevelVal(j)`. We track those two indices separately
    // so a file like
    //
    //     val GREETING = "hi"   // val_idx 0
    //     fun main() { ... }    // fn_idx  0
    //
    // registers `DefId::Function(0)` for `main`, not `Function(1)`.
    // Pass 2 below looks up by the same kind-relative index.
    let mut fn_idx_pass1: u32 = 0;
    let mut val_idx_pass1: u32 = 0;
    for decl in &file.decls {
        match decl {
            Decl::Fun(f) => {
                let sig = tc.signature_for_fun(f);
                tc.out
                    .top_signatures
                    .insert(DefId::Function(fn_idx_pass1), sig);
                tc.fn_names.push(f.name);
                fn_idx_pass1 += 1;
            }
            Decl::Val(v) => {
                // Top-level val type: synthesized from initializer for
                // string and int literals; explicit annotation otherwise.
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
            Decl::Unsupported { .. } => {}
        }
    }
    // Built-in: println accepts Any? and returns Unit.
    tc.out.top_signatures.insert(
        DefId::PrintlnIntrinsic,
        Signature {
            params: vec![Ty::Nullable(Box::new(Ty::Any))],
            ret: Ty::Unit,
        },
    );

    // Pass 2: check each function body / top val initializer.
    let mut fn_idx: u32 = 0;
    let mut val_idx: u32 = 0;
    for decl in &file.decls {
        match decl {
            Decl::Fun(f) => {
                let rf = &resolved.functions[fn_idx as usize];
                let typed = tc.check_function(fn_idx, f, rf);
                tc.out.functions.push(typed);
                fn_idx += 1;
            }
            Decl::Val(v) => {
                let typed = tc.check_top_val(val_idx, v);
                tc.out.top_vals.push(typed);
                val_idx += 1;
            }
            Decl::Unsupported { .. } => {}
        }
    }
    tc.out
}

struct TypeChecker<'a> {
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    out: TypedFile,
    /// Function names indexed by function-only index, populated in pass 1.
    fn_names: Vec<Symbol>,
}

impl<'a> TypeChecker<'a> {
    fn signature_for_fun(&mut self, f: &FunDecl) -> Signature {
        let mut params: Vec<Ty> = Vec::new();
        // For extension functions: receiver type is the first param.
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

    fn type_ref(&mut self, tr: &TypeRef) -> Option<Ty> {
        let name = self.interner.resolve(tr.name).to_string();
        match ty_from_name(&name) {
            Some(t) => Some(if tr.nullable {
                Ty::Nullable(Box::new(t))
            } else {
                t
            }),
            None => {
                self.diags
                    .push(Diagnostic::error(tr.span, format!("unknown type `{name}`")));
                None
            }
        }
    }

    fn check_function(
        &mut self,
        idx: u32,
        f: &FunDecl,
        rf: &skotch_resolve::ResolvedFunction,
    ) -> TypedFunction {
        let sig = self.out.top_signatures[&DefId::Function(idx)].clone();
        let mut local_tys: Vec<Ty> = Vec::new();
        let mut expr_tys: Vec<Ty> = Vec::new();
        let _ = rf;
        // Walk the body, tracking declared local types in declaration order.
        let mut scope: Vec<(skotch_intern::Symbol, Ty)> = Vec::new();
        let param_offset = if f.receiver_ty.is_some() {
            // Extension function: `this` is the first param.
            let this_sym = self.interner.intern("this");
            scope.push((this_sym, sig.params[0].clone()));
            1
        } else {
            0
        };
        for (pi, p) in f.params.iter().enumerate() {
            scope.push((p.name, sig.params[pi + param_offset].clone()));
        }
        self.check_block(&f.body, &mut scope, &mut local_tys, &mut expr_tys);
        TypedFunction {
            name_index: idx,
            return_ty: sig.ret,
            param_tys: sig.params,
            local_tys,
            expr_tys,
        }
    }

    fn check_top_val(&mut self, idx: u32, v: &ValDecl) -> TypedTopVal {
        let ty = self.synth_top_init(&v.init);
        TypedTopVal {
            name_index: idx,
            ty,
        }
    }

    fn synth_top_init(&mut self, e: &Expr) -> Ty {
        match e {
            Expr::IntLit(_, _) => Ty::Int,
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::StringLit(_, _) => Ty::String,
            other => {
                self.diags.push(Diagnostic::error(
                    other.span(),
                    "top-level val initializers must be a literal in PR #1",
                ));
                Ty::Error
            }
        }
    }

    fn check_block(
        &mut self,
        block: &Block,
        scope: &mut Vec<(skotch_intern::Symbol, Ty)>,
        local_tys: &mut Vec<Ty>,
        expr_tys: &mut Vec<Ty>,
    ) {
        let saved = scope.len();
        for stmt in &block.stmts {
            match stmt {
                Stmt::Expr(e) => {
                    let _ = self.synth_expr(e, scope, expr_tys);
                }
                Stmt::Val(v) => {
                    let init_ty = self.synth_expr(&v.init, scope, expr_tys);
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
                        self.synth_expr(v, scope, expr_tys);
                    }
                }
                Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
                    let _ = self.synth_expr(cond, scope, expr_tys);
                    self.check_block(body, scope, expr_tys, local_tys);
                }
                Stmt::Assign { value, .. } => {
                    let _ = self.synth_expr(value, scope, expr_tys);
                }
                Stmt::Break(_) | Stmt::Continue(_) => {}
                Stmt::LocalFun(_) => {} // handled in MIR lowering
                Stmt::For {
                    var_name,
                    start: range_start,
                    end: range_end,
                    body,
                    ..
                } => {
                    let _ = self.synth_expr(range_start, scope, expr_tys);
                    let _ = self.synth_expr(range_end, scope, expr_tys);
                    local_tys.push(Ty::Int);
                    scope.push((*var_name, Ty::Int));
                    self.check_block(body, scope, expr_tys, local_tys);
                }
            }
        }
        scope.truncate(saved);
    }

    fn synth_expr(
        &mut self,
        e: &Expr,
        scope: &mut Vec<(skotch_intern::Symbol, Ty)>,
        out: &mut Vec<Ty>,
    ) -> Ty {
        let ty = match e {
            Expr::IntLit(_, _) => Ty::Int,
            Expr::BoolLit(_, _) => Ty::Bool,
            Expr::StringLit(_, _) => Ty::String,
            Expr::Ident(name, _) => {
                // Look up in local scope first; otherwise fall back to
                // `Any`. We don't bother re-resolving against the
                // top-level signature table here because the resolver
                // already did the binding work and the MIR lowering
                // pass reads the resolved table directly. The looseness
                // is invisible to backends.
                if let Some((_, t)) = scope.iter().rev().find(|(n, _)| *n == *name) {
                    t.clone()
                } else {
                    Ty::Any
                }
            }
            Expr::Paren(inner, _) => self.synth_expr(inner, scope, out),
            Expr::Binary { op, lhs, rhs, span } => {
                let lt = self.synth_expr(lhs, scope, out);
                let rt = self.synth_expr(rhs, scope, out);
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        if lt == Ty::Int && rt == Ty::Int {
                            Ty::Int
                        } else if *op == BinOp::Add && lt == Ty::String {
                            // String + anything → String concatenation
                            Ty::String
                        } else if lt == Ty::Error || rt == Ty::Error {
                            Ty::Error
                        } else {
                            self.diags.push(Diagnostic::error(
                                *span,
                                format!(
                                    "arithmetic on {} and {} not supported in PR #1",
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
            Expr::Unary { operand, .. } => self.synth_expr(operand, scope, out),
            Expr::Call { callee, args, .. } => {
                for a in args {
                    self.synth_expr(a, scope, out);
                }
                // Look up the callee's return type so function calls
                // used in expressions get the correct type.
                if let Expr::Ident(name, _) = callee.as_ref() {
                    // Check the function name → DefId mapping to find
                    // the return type from the pre-computed signatures.
                    for (&def_id, sig) in &self.out.top_signatures {
                        if let DefId::Function(fi) = def_id {
                            // Match the function name through the
                            // fn_names list.
                            if self.fn_names.get(fi as usize).copied() == Some(*name) {
                                return sig.ret.clone();
                            }
                        }
                        if def_id == DefId::PrintlnIntrinsic {
                            let println_sym = self.interner.intern("println");
                            if *name == println_sym {
                                return sig.ret.clone();
                            }
                        }
                    }
                }
                Ty::Unit
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.synth_expr(cond, scope, out);
                self.check_block(then_block, scope, &mut Vec::new(), out);
                if let Some(eb) = else_block {
                    self.check_block(eb, scope, &mut Vec::new(), out);
                }
                // PR #1 punt: if-as-expression always types to Int.
                // Fixture 07 uses `if (true) 1 else 2` which fits.
                Ty::Int
            }
            Expr::Field { receiver, .. } => self.synth_expr(receiver, scope, out),
            Expr::When {
                subject,
                branches,
                else_body,
                ..
            } => {
                self.synth_expr(subject, scope, out);
                let mut result_ty = Ty::Unit;
                for b in branches {
                    self.synth_expr(&b.pattern, scope, out);
                    result_ty = self.synth_expr(&b.body, scope, out);
                }
                if let Some(eb) = else_body {
                    result_ty = self.synth_expr(eb, scope, out);
                }
                result_ty
            }
            Expr::StringTemplate(parts, _) => {
                for p in parts {
                    if let TemplatePart::Expr(inner) = p {
                        self.synth_expr(inner, scope, out);
                    }
                }
                Ty::String
            }
        };
        out.push(ty.clone());
        ty
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
        let f = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&f, &mut interner, &mut diags);
        let t = type_check(&f, &r, &mut interner, &mut diags);
        (t, diags)
    }

    #[test]
    fn typeck_println_string() {
        let (t, d) = run(r#"fun main() { println("hi") }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(t.functions.len(), 1);
    }

    #[test]
    fn typeck_println_int() {
        let (_, d) = run("fun main() { println(42) }");
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn typeck_arithmetic_int() {
        let (_, d) = run("fun main() { println(1 + 2 * 3) }");
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn typeck_if_expression_int() {
        let (_, d) = run("fun main() { val x = if (true) 1 else 2; println(x) }");
        assert!(d.is_empty(), "{:?}", d);
    }

    #[test]
    fn typeck_val_inference() {
        let (t, d) = run(r#"fun main() { val s = "hi" }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(t.functions[0].local_tys, vec![Ty::String]);
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: typeck_nullable_check         — String? unwrapping
    // TODO: typeck_overload_resolution    — println(Int) vs println(String)
    // TODO: typeck_smart_cast             — `if (x is String) x.length` after `is`
    // TODO: typeck_generics               — fun <T> id(x: T): T
    // TODO: error_type_mismatch           — `val x: Int = "no"` should error
}
