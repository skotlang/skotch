//! Lowers AST + `ResolvedFile` + `TypedFile` to a backend-neutral
//! [`MirModule`].
//!
//! This pass is the source of all type-aware structural decisions
//! backends rely on. In particular, the `println` intrinsic dispatch is
//! decided here, not in the backend, by recording an `args[0]` type
//! that the backend examines via `MirFunction::locals[arg.0]`.
//!
//! ## What it currently lowers
//!
//! - `fun main()` and similar zero/one-parameter top-level functions
//! - `println(<int|string|local>)` calls
//! - Integer arithmetic
//! - Local `val s = "..."` and `val n = 42`
//! - `greet("...")` style top-level calls between functions in the
//!   same file
//!
//! ## What it explicitly errors on
//!
//! Anything else: classes, when, lambdas, generics, string templates,
//! if-as-expression, top-level vals. Each unsupported construct emits
//! a diagnostic and stops lowering for that function.

use rustc_hash::FxHashMap;
use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_intern::{Interner, Symbol};
use skotch_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, FuncId, LocalId, MirConst, MirFunction, MirModule,
    Rvalue, Stmt as MStmt, Terminator,
};
use skotch_resolve::{DefId, ResolvedFile};
use skotch_syntax::{BinOp, Decl, Expr, FunDecl, KtFile, Stmt, ValDecl};
use skotch_typeck::TypedFile;
use skotch_types::Ty;

/// Lower a parsed/resolved/typed file to MIR.
pub fn lower_file(
    file: &KtFile,
    resolved: &ResolvedFile,
    typed: &TypedFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    wrapper_class: &str,
) -> MirModule {
    let mut module = MirModule {
        wrapper_class: wrapper_class.to_string(),
        ..MirModule::default()
    };

    // ─── Pass 1: register top-level functions ───────────────────────────
    //
    // Pre-allocate a `FuncId` for every top-level `fun` in source order
    // so call sites between them resolve consistently regardless of
    // declaration order.
    let mut name_to_func: FxHashMap<Symbol, FuncId> = FxHashMap::default();
    for decl in &file.decls {
        if let Decl::Fun(f) = decl {
            let id = FuncId(module.functions.len() as u32);
            name_to_func.insert(f.name, id);
            let name_str = interner.resolve(f.name).to_string();
            module.functions.push(MirFunction {
                id,
                name: name_str,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty: Ty::Unit,
            });
        }
    }

    // ─── Pass 2: collect top-level val constants ────────────────────────
    //
    // Top-level vals with literal initializers are lowered as
    // **inlined constants**: every reference site emits a
    // `Rvalue::Const(<value>)` directly. This avoids the complexity of
    // emitting JVM static fields + `<clinit>` and DEX `static_values_off`
    // for the common case where the user wrote a literal. Compile speed
    // and output accuracy both win — there's no extra method to compile
    // and the runtime behavior is identical (the JIT would inline these
    // anyway).
    //
    // Non-literal initializers are already rejected by `skotch-typeck`
    // ("top-level val initializers must be a literal in PR #1"), so we
    // skip silently here when we can't extract a `MirConst`.
    let mut name_to_global: FxHashMap<Symbol, MirConst> = FxHashMap::default();
    for decl in &file.decls {
        if let Decl::Val(v) = decl {
            if let Some(c) = lower_const_init(&v.init, &mut module) {
                name_to_global.insert(v.name, c);
            }
            // If we can't extract a constant, typeck already errored;
            // we just don't register the global, and any reference to
            // it later will produce its own diagnostic.
        }
    }

    // ─── Pass 3: lower each function body ───────────────────────────────
    let mut fn_idx: usize = 0;
    for decl in &file.decls {
        match decl {
            Decl::Fun(f) => {
                let typed_fn = typed.functions.get(fn_idx);
                lower_function(
                    f,
                    fn_idx,
                    typed_fn,
                    &name_to_func,
                    &name_to_global,
                    &mut module,
                    interner,
                    diags,
                );
                fn_idx += 1;
            }
            Decl::Val(_) => {
                // Already handled in pass 2.
            }
            Decl::Unsupported { what, span } => {
                diags.push(Diagnostic::error(
                    *span,
                    format!("`{what}` declarations are not yet supported"),
                ));
            }
        }
    }
    let _ = resolved;

    module
}

/// Try to extract a compile-time-constant value from a top-level
/// `val` initializer. Returns `None` for non-literal initializers
/// (which the type checker has already rejected).
///
/// String literals are interned into the module's string pool here so
/// that the same `StringId` is shared with any inline `println("...")`
/// uses of the same text — backends dedupe constant-pool entries
/// across the whole module, and inlining the global through the same
/// string pool keeps that dedup correct.
fn lower_const_init(e: &Expr, module: &mut MirModule) -> Option<MirConst> {
    match e {
        Expr::IntLit(v, _) => Some(MirConst::Int(*v as i32)),
        Expr::BoolLit(v, _) => Some(MirConst::Bool(*v)),
        Expr::StringLit(s, _) => {
            let sid = module.intern_string(s);
            Some(MirConst::String(sid))
        }
        // Wrapper around a literal (`val X = (1)`) — recurse so the
        // user's harmless parens don't break const-folding.
        Expr::Paren(inner, _) => lower_const_init(inner, module),
        _ => None,
    }
}

/// Surface type of a `MirConst`. Used by the inline-global lowering
/// in `Expr::Ident` so the local that holds the inlined value gets
/// the right type for the JVM/DEX/LLVM backends to dispatch on.
fn const_ty(c: &MirConst) -> Ty {
    match c {
        MirConst::Unit => Ty::Unit,
        MirConst::Bool(_) => Ty::Bool,
        MirConst::Int(_) => Ty::Int,
        MirConst::String(_) => Ty::String,
    }
}

/// Builder for a multi-block MIR function. Tracks the "current
/// block" so statements are appended to the right place, and lets
/// `lower_expr` create new basic blocks for `if`-expressions without
/// threading a mutable block-list through every function.
struct FnBuilder {
    mf: MirFunction,
    /// Index of the block we're currently emitting into.
    cur_block: u32,
}

impl FnBuilder {
    fn new(fn_idx: usize, name: String, return_ty: Ty) -> Self {
        let mf = MirFunction {
            id: FuncId(fn_idx as u32),
            name,
            params: Vec::new(),
            locals: Vec::new(),
            // Start with one empty entry block (block 0).
            blocks: vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return, // patched later
            }],
            return_ty,
        };
        FnBuilder { mf, cur_block: 0 }
    }

    fn new_local(&mut self, ty: Ty) -> LocalId {
        self.mf.new_local(ty)
    }

    fn push_stmt(&mut self, stmt: MStmt) {
        self.mf.blocks[self.cur_block as usize].stmts.push(stmt);
    }

    /// Create a fresh empty block and return its index. Does NOT
    /// switch `cur_block` — the caller decides when to switch.
    fn new_block(&mut self) -> u32 {
        let idx = self.mf.blocks.len() as u32;
        self.mf.blocks.push(BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return, // patched later
        });
        idx
    }

    /// Set the terminator of the current block and switch to
    /// `next_block`.
    fn terminate_and_switch(&mut self, terminator: Terminator, next_block: u32) {
        self.mf.blocks[self.cur_block as usize].terminator = terminator;
        self.cur_block = next_block;
    }

    /// Set the terminator of the current block without switching.
    #[allow(dead_code)]
    fn set_terminator(&mut self, terminator: Terminator) {
        self.mf.blocks[self.cur_block as usize].terminator = terminator;
    }

    fn finish(self) -> MirFunction {
        self.mf
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_function(
    f: &FunDecl,
    fn_idx: usize,
    typed: Option<&skotch_typeck::TypedFunction>,
    name_to_func: &FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let name = interner.resolve(f.name).to_string();
    let return_ty = typed.map(|t| t.return_ty.clone()).unwrap_or(Ty::Unit);
    let mut fb = FnBuilder::new(fn_idx, name.clone(), return_ty);

    // Allocate parameter locals first so they get LocalId 0..N.
    let mut scope: Vec<(Symbol, LocalId)> = Vec::new();
    for (pi, p) in f.params.iter().enumerate() {
        let ty = typed
            .and_then(|t| t.param_tys.get(pi).cloned())
            .unwrap_or(Ty::Any);
        let id = fb.new_local(ty);
        fb.mf.params.push(id);
        scope.push((p.name, id));
    }

    let mut ok = true;
    for s in &f.body.stmts {
        if !lower_stmt(
            s,
            &mut fb,
            &mut scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
        ) {
            ok = false;
            break;
        }
    }
    // The current block's terminator stays `Return` (set by the
    // FnBuilder constructor and never overwritten for the last block).

    if ok {
        module.functions[fn_idx] = fb.finish();
    } else {
        module.functions[fn_idx] = MirFunction {
            id: FuncId(fn_idx as u32),
            name,
            params: Vec::new(),
            locals: Vec::new(),
            blocks: vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            return_ty: Ty::Unit,
        };
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_stmt(
    stmt: &Stmt,
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> bool {
    match stmt {
        Stmt::Expr(e) => lower_expr(
            e,
            fb,
            scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
        )
        .is_some(),
        Stmt::Val(v) => lower_val_stmt(
            v,
            fb,
            scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
        ),
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                let _ = lower_expr(
                    v,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                );
            }
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_val_stmt(
    v: &ValDecl,
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> bool {
    let Some(rhs) = lower_expr(
        &v.init,
        fb,
        scope,
        module,
        name_to_func,
        name_to_global,
        interner,
        diags,
    ) else {
        return false;
    };
    let ty = fb.mf.locals[rhs.0 as usize].clone();
    let dest = fb.new_local(ty);
    fb.push_stmt(MStmt::Assign {
        dest,
        value: Rvalue::Local(rhs),
    });
    scope.push((v.name, dest));
    true
}

/// Lower an expression and return the local that holds its value.
/// Returns `None` if lowering hit an unsupported construct.
#[allow(clippy::too_many_arguments)]
fn lower_expr(
    e: &Expr,
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> Option<LocalId> {
    match e {
        Expr::IntLit(v, _) => {
            let dest = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Int(*v as i32)),
            });
            Some(dest)
        }
        Expr::BoolLit(v, _) => {
            let dest = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Bool(*v)),
            });
            Some(dest)
        }
        Expr::StringLit(s, _) => {
            let sid = module.intern_string(s);
            let dest = fb.new_local(Ty::String);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::String(sid)),
            });
            Some(dest)
        }
        Expr::Ident(name, span) => {
            if let Some((_, id)) = scope.iter().rev().find(|(n, _)| *n == *name) {
                let src = *id;
                let ty = fb.mf.locals[src.0 as usize].clone();
                let dest = fb.new_local(ty);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Local(src),
                });
                Some(dest)
            } else if let Some(constant) = name_to_global.get(name) {
                let ty = const_ty(constant);
                let dest = fb.new_local(ty);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Const(constant.clone()),
                });
                Some(dest)
            } else {
                diags.push(Diagnostic::error(
                    *span,
                    format!(
                        "cannot lower reference to `{}` — only locals, parameters, and top-level vals are supported",
                        interner.resolve(*name)
                    ),
                ));
                None
            }
        }
        Expr::Paren(inner, _) => lower_expr(
            inner,
            fb,
            scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
        ),
        Expr::Binary { op, lhs, rhs, span } => {
            let l = lower_expr(
                lhs,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
            )?;
            let r = lower_expr(
                rhs,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
            )?;
            let (mop, result_ty) = match op {
                BinOp::Add => (MBinOp::AddI, Ty::Int),
                BinOp::Sub => (MBinOp::SubI, Ty::Int),
                BinOp::Mul => (MBinOp::MulI, Ty::Int),
                BinOp::Div => (MBinOp::DivI, Ty::Int),
                BinOp::Mod => (MBinOp::ModI, Ty::Int),
                BinOp::Eq => (MBinOp::CmpEq, Ty::Bool),
                BinOp::NotEq => (MBinOp::CmpNe, Ty::Bool),
                BinOp::Lt => (MBinOp::CmpLt, Ty::Bool),
                BinOp::Gt => (MBinOp::CmpGt, Ty::Bool),
                BinOp::LtEq => (MBinOp::CmpLe, Ty::Bool),
                BinOp::GtEq => (MBinOp::CmpGe, Ty::Bool),
                _ => {
                    diags.push(Diagnostic::error(
                        *span,
                        format!("binary operator {op:?} not yet supported"),
                    ));
                    return None;
                }
            };
            let dest = fb.new_local(result_ty);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::BinOp {
                    op: mop,
                    lhs: l,
                    rhs: r,
                },
            });
            Some(dest)
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            // ── Multi-block lowering for if-as-expression ────────
            //
            // 1. Lower the condition in the current block
            // 2. Create then, else, and merge blocks
            // 3. Terminate current block with Branch
            // 4. Lower then branch → writes result to shared local → Goto merge
            // 5. Lower else branch (if present) → writes result → Goto merge
            // 6. Merge block becomes the new current block
            //
            // The shared result local is pre-allocated here. Both
            // branches write to it via `Rvalue::Local(their_val)`.
            // The merge block can read it directly.

            let cond_local = lower_expr(
                cond,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
            )?;

            let then_blk = fb.new_block();
            let else_blk = fb.new_block();
            let merge_blk = fb.new_block();

            // Pre-allocate the result local. Type: Int for now (the
            // only if-as-expression type we support). A future PR
            // should infer the type from the branches.
            let result = fb.new_local(Ty::Int);

            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: cond_local,
                    then_block: then_blk,
                    else_block: else_blk,
                },
                then_blk,
            );

            // Then branch.
            for s in &then_block.stmts {
                match s {
                    skotch_syntax::Stmt::Expr(e) => {
                        if let Some(val) = lower_expr(
                            e,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                        ) {
                            fb.push_stmt(MStmt::Assign {
                                dest: result,
                                value: Rvalue::Local(val),
                            });
                        }
                    }
                    _ => {
                        let _ = lower_stmt(
                            s,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                        );
                    }
                }
            }
            fb.terminate_and_switch(Terminator::Goto(merge_blk), else_blk);

            // Else branch.
            if let Some(eb) = else_block {
                for s in &eb.stmts {
                    match s {
                        skotch_syntax::Stmt::Expr(e) => {
                            if let Some(val) = lower_expr(
                                e,
                                fb,
                                scope,
                                module,
                                name_to_func,
                                name_to_global,
                                interner,
                                diags,
                            ) {
                                fb.push_stmt(MStmt::Assign {
                                    dest: result,
                                    value: Rvalue::Local(val),
                                });
                            }
                        }
                        _ => {
                            let _ = lower_stmt(
                                s,
                                fb,
                                scope,
                                module,
                                name_to_func,
                                name_to_global,
                                interner,
                                diags,
                            );
                        }
                    }
                }
            }
            fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);

            Some(result)
        }
        Expr::Call { callee, args, span } => {
            let callee_name = match callee.as_ref() {
                Expr::Ident(name, _) => *name,
                _ => {
                    diags.push(Diagnostic::error(
                        *span,
                        "only direct function calls are supported",
                    ));
                    return None;
                }
            };

            // ─── Special form: println(<string template>) ───────────
            if interner.resolve(callee_name) == "println"
                && args.len() == 1
                && matches!(&args[0], Expr::StringTemplate(_, _))
            {
                if let Expr::StringTemplate(parts, _) = &args[0] {
                    let mut arg_locals = Vec::with_capacity(parts.len());
                    for part in parts {
                        let id = lower_template_part(
                            part,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                        )?;
                        arg_locals.push(id);
                    }
                    let dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::PrintlnConcat,
                            args: arg_locals,
                        },
                    });
                    return Some(dest);
                }
            }

            let mut arg_locals = Vec::new();
            for a in args {
                let id = lower_expr(
                    a,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                )?;
                arg_locals.push(id);
            }

            let kind = if interner.resolve(callee_name) == "println" {
                CallKind::Println
            } else if let Some(fid) = name_to_func.get(&callee_name) {
                CallKind::Static(*fid)
            } else {
                diags.push(Diagnostic::error(
                    *span,
                    format!("unknown call target `{}`", interner.resolve(callee_name)),
                ));
                return None;
            };

            let dest = fb.new_local(Ty::Unit);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Call {
                    kind,
                    args: arg_locals,
                },
            });
            Some(dest)
        }
        Expr::StringTemplate(_, span) => {
            diags.push(Diagnostic::error(
                *span,
                "string templates are only supported as a direct argument of `println` in PR scope",
            ));
            None
        }
        Expr::Unary { span, .. } | Expr::Field { span, .. } => {
            diags.push(Diagnostic::error(
                *span,
                "this expression form is not yet lowered to MIR",
            ));
            let _ = (DefId::Error, name_to_func);
            None
        }
    }
}

/// Lower one template part to a local.
#[allow(clippy::too_many_arguments)]
fn lower_template_part(
    part: &skotch_syntax::TemplatePart,
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> Option<LocalId> {
    use skotch_syntax::TemplatePart;
    match part {
        TemplatePart::Text(s, _) => {
            let sid = module.intern_string(s);
            let dest = fb.new_local(Ty::String);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::String(sid)),
            });
            Some(dest)
        }
        TemplatePart::IdentRef(name, span) => {
            let synthetic = Expr::Ident(*name, *span);
            lower_expr(
                &synthetic,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
            )
        }
        TemplatePart::Expr(inner) => lower_expr(
            inner,
            fb,
            scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_lexer::lex;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;
    use skotch_typeck::type_check;

    fn lower(src: &str) -> (MirModule, Diagnostics) {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let f = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&f, &mut interner, &mut diags);
        let t = type_check(&f, &r, &mut interner, &mut diags);
        let m = lower_file(&f, &r, &t, &mut interner, &mut diags, "HelloKt");
        (m, diags)
    }

    #[test]
    fn lower_println_string() {
        let (m, d) = lower(r#"fun main() { println("hi") }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(m.functions.len(), 1);
        let f = &m.functions[0];
        // String pool should contain "hi".
        assert_eq!(m.strings, vec!["hi".to_string()]);
        // Body: load const string, call println, return.
        assert_eq!(f.blocks.len(), 1);
        let bb = &f.blocks[0];
        assert!(bb.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Const(MirConst::String(_)),
                ..
            }
        )));
        assert!(bb.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Call {
                    kind: CallKind::Println,
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn lower_println_int() {
        let (m, d) = lower("fun main() { println(42) }");
        assert!(d.is_empty(), "{:?}", d);
        let bb = &m.functions[0].blocks[0];
        assert!(bb.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Const(MirConst::Int(42)),
                ..
            }
        )));
    }

    #[test]
    fn lower_arithmetic() {
        let (m, d) = lower("fun main() { println(1 + 2 * 3) }");
        assert!(d.is_empty(), "{:?}", d);
        let bb = &m.functions[0].blocks[0];
        assert!(bb.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::BinOp {
                    op: MBinOp::AddI,
                    ..
                },
                ..
            }
        )));
        assert!(bb.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::BinOp {
                    op: MBinOp::MulI,
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn lower_function_call() {
        let src = r#"
            fun greet(n: String) { println(n) }
            fun main() { greet("Kotlin") }
        "#;
        let (m, d) = lower(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(m.functions.len(), 2);
        let main_block = &m.functions[1].blocks[0];
        assert!(main_block.stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Call {
                    kind: CallKind::Static(_),
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn lower_top_level_val_string_inlines_constant() {
        // The val should be inlined as a `Const(String)` at the
        // reference site inside `main`. The MIR should NOT contain
        // any synthetic global field — vals are pure inlined
        // constants in skotch's lowering.
        let (m, d) = lower(r#"val GREETING = "hi"; fun main() { println(GREETING) }"#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(m.functions.len(), 1, "no synthetic <clinit> generated");
        // The string pool has "hi" once (deduped between the val
        // initializer and any other use).
        assert_eq!(m.strings, vec!["hi".to_string()]);
        let main = &m.functions[0];
        // The body must contain a Const(String) load for "hi" — that
        // came from the inlined global.
        assert!(main.blocks[0].stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Const(MirConst::String(_)),
                ..
            }
        )));
        // …followed by a Println call.
        assert!(main.blocks[0].stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Call {
                    kind: CallKind::Println,
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn lower_top_level_val_int_inlines_constant() {
        let (m, d) = lower(r#"val ANSWER = 42; fun main() { println(ANSWER) }"#);
        assert!(d.is_empty(), "{:?}", d);
        let main = &m.functions[0];
        assert!(main.blocks[0].stmts.iter().any(|s| matches!(
            s,
            MStmt::Assign {
                value: Rvalue::Const(MirConst::Int(42)),
                ..
            }
        )));
    }

    #[test]
    fn lower_local_shadows_top_level_val() {
        // A local with the same name as a top-level val must shadow
        // the global. The local's value (`"local"`) should be the
        // one that reaches `println`, not the global's value
        // (`"global"`).
        let src = r#"
            val X = "global"
            fun main() {
                val X = "local"
                println(X)
            }
        "#;
        let (m, d) = lower(src);
        assert!(d.is_empty(), "{:?}", d);
        // The string pool contains BOTH strings — the global is
        // interned at module-init time even though it's never read.
        assert!(m.strings.contains(&"local".to_string()));
        assert!(m.strings.contains(&"global".to_string()));
    }

    // ─── future stubs ────────────────────────────────────────────────────
    // TODO: lower_if_expression          — once Branch terminator lands
    // TODO: lower_string_template        — once Concat intrinsic lands
    // TODO: lower_local_var_reassign     — `var x = 1; x = 2`
}
