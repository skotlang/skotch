//! Name resolution for the Kotlin subset skotch accepts.
//!
//! Walks a [`KtFile`] AST, builds a flat scope tree, and resolves
//! identifier references to [`DefId`]s. PR #1 supports only:
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
use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_span::Span;
use skotch_syntax::{Block, Decl, Expr, FunDecl, KtFile, Param, Stmt, TemplatePart, ValDecl};

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

/// Build a [`ResolvedFile`] from a parsed AST.
pub fn resolve_file(
    file: &KtFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
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
    // Register stdlib top-level functions.
    for name in &[
        "maxOf",
        "minOf",
        "with",
        "repeat",
        "listOf",
        "mutableListOf",
        "Pair",
        "Triple",
    ] {
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
                iterable,
                body,
                ..
            } => {
                self.resolve_expr(fn_idx, iterable, scope, rf);
                let local_idx = rf.locals.len() as u32;
                rf.locals.push(*var_name);
                scope.push((*var_name, DefId::Local(fn_idx, local_idx)));
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                // Resolve the target — it must already be declared.
                let _def = lookup(scope, *target).unwrap_or_else(|| {
                    self.diags.push(Diagnostic::error(
                        *span,
                        format!("unresolved identifier `{}`", self.interner.resolve(*target)),
                    ));
                    DefId::Local(fn_idx, 0)
                });
                self.resolve_expr(fn_idx, value, scope, rf);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
            Stmt::TryStmt {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in &body.stmts {
                    self.resolve_stmt(fn_idx, s, scope, rf);
                }
                if let Some(cb) = catch_body {
                    for s in &cb.stmts {
                        self.resolve_stmt(fn_idx, s, scope, rf);
                    }
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
                // Resolve the function body.
                let inner_rf = self.resolve_function(fn_idx, f);
                let _ = inner_rf; // body refs handled separately
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
        match expr {
            Expr::IntLit(_, _)
            | Expr::LongLit(_, _)
            | Expr::DoubleLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::NullLit(_)
            | Expr::StringLit(_, _) => {}
            Expr::Lambda { params, body, .. } => {
                let saved = scope.len();
                for p in params {
                    scope.push((p.name, DefId::PossibleExternal(p.name)));
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
                        self.diags.push(Diagnostic::error(
                            *span,
                            format!("unresolved identifier `{name_str}`"),
                        ));
                        DefId::Error
                    })
                });
                rf.body_refs.push(ResolvedRef { span: *span, def });
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr(fn_idx, callee, scope, rf);
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
            | Expr::LongLit(_, _)
            | Expr::DoubleLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::NullLit(_)
            | Expr::StringLit(_, _) => {}
            Expr::Ident(name, span) => {
                let def = self.out.top_level.get(name).copied().unwrap_or_else(|| {
                    self.diags.push(Diagnostic::error(
                        *span,
                        format!(
                            "unresolved top-level identifier `{}`",
                            self.interner.resolve(*name)
                        ),
                    ));
                    DefId::Error
                });
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
                // PR #1 doesn't lower if-expressions inside top-level
                // initializers; we'd need to materialize a <clinit>
                // basic block. Punt with a clear diagnostic.
                self.diags.push(Diagnostic::error(
                    expr.span(),
                    "if-expressions in top-level val initializers are not yet supported",
                ));
            }
            Expr::StringTemplate(_, _) => {
                self.diags.push(Diagnostic::error(
                    expr.span(),
                    "string templates in top-level val initializers are not yet supported",
                ));
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
        let r = resolve_file(&file, &mut interner, &mut diags);
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
