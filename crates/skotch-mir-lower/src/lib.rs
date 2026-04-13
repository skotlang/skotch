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
    BasicBlock, BinOp as MBinOp, CallKind, FuncId, LocalId, MirClass, MirConst, MirField,
    MirFunction, MirModule, Rvalue, Stmt as MStmt, Terminator,
};
use skotch_resolve::ResolvedFile;
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
    let mut fn_pass1_idx: usize = 0;
    for decl in &file.decls {
        if let Decl::Fun(f) = decl {
            let id = FuncId(module.functions.len() as u32);
            name_to_func.insert(f.name, id);
            let name_str = interner.resolve(f.name).to_string();
            // Use the typechecker's return type so recursive calls and
            // forward references get the correct type, not Ty::Unit.
            let return_ty = typed
                .functions
                .get(fn_pass1_idx)
                .map(|t| t.return_ty.clone())
                .unwrap_or(Ty::Unit);
            let required = f.params.iter().filter(|p| p.default.is_none()).count();
            let param_defaults: Vec<Option<MirConst>> = f
                .params
                .iter()
                .map(|p| {
                    p.default
                        .as_ref()
                        .and_then(|d| lower_const_init(d, &mut module))
                })
                .collect();
            let param_names: Vec<String> = f
                .params
                .iter()
                .map(|p| interner.resolve(p.name).to_string())
                .collect();
            module.functions.push(MirFunction {
                id,
                name: name_str,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty,
                required_params: required,
                param_names,
                param_defaults,
                is_abstract: false,
            });
            fn_pass1_idx += 1;
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

    // ─── Build import map ────────────────────────────────────────────────
    // Maps simple class names → JVM class paths from import statements.
    // Also includes default java.lang.* imports (Kotlin implicitly imports java.lang.*).
    let mut import_map: FxHashMap<String, String> = FxHashMap::default();
    // Default java.lang.* imports
    for name in &[
        "System",
        "Math",
        "Integer",
        "Long",
        "Double",
        "Boolean",
        "String",
        "Thread",
        "Runtime",
        "Object",
        "Class",
        "Comparable",
    ] {
        import_map.insert(name.to_string(), format!("java/lang/{name}"));
    }
    // Process explicit imports
    for imp in &file.imports {
        let segments: Vec<&str> = imp.path.iter().map(|s| interner.resolve(*s)).collect();
        if imp.is_wildcard {
            // `import foo.bar.*` — we can't enumerate classes, but store the package prefix
            // for future classpath scanning. For now, no-op.
        } else if !segments.is_empty() {
            let simple_name = segments.last().unwrap().to_string();
            let jvm_path = segments.join("/");
            import_map.insert(simple_name, jvm_path);
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
                    &mut name_to_func,
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
            Decl::Class(c) => {
                lower_class(
                    c,
                    &mut name_to_func,
                    &name_to_global,
                    &mut module,
                    interner,
                    diags,
                );
            }
            Decl::Object(o) => {
                lower_object(
                    o,
                    &mut name_to_func,
                    &name_to_global,
                    &mut module,
                    interner,
                    diags,
                );
            }
            Decl::Enum(e) => {
                lower_enum(e, &mut name_to_func, &mut module, interner);
            }
            Decl::Interface(iface) => {
                lower_interface(iface, &mut module, interner);
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
        Expr::LongLit(v, _) => Some(MirConst::Long(*v)),
        Expr::DoubleLit(v, _) => Some(MirConst::Double(*v)),
        Expr::BoolLit(v, _) => Some(MirConst::Bool(*v)),
        Expr::NullLit(_) => Some(MirConst::Null),
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
        MirConst::Long(_) => Ty::Long,
        MirConst::Double(_) => Ty::Double,
        MirConst::Null => Ty::Nullable(Box::new(Ty::Any)),
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
            blocks: vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            return_ty,
            required_params: 0,
            param_names: Vec::new(),
            param_defaults: Vec::new(),
            is_abstract: false,
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
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let name = interner.resolve(f.name).to_string();
    let return_ty = typed
        .map(|t| t.return_ty.clone())
        .or_else(|| {
            f.return_ty
                .as_ref()
                .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
        })
        .unwrap_or(Ty::Unit);
    let mut fb = FnBuilder::new(fn_idx, name.clone(), return_ty);

    // Allocate parameter locals first so they get LocalId 0..N.
    let mut scope: Vec<(Symbol, LocalId)> = Vec::new();

    // For extension functions: add the receiver as the first parameter.
    // It's accessible as `this` in the function body.
    if let Some(recv) = &f.receiver_ty {
        let recv_ty = skotch_types::ty_from_name(interner.resolve(recv.name)).unwrap_or(Ty::Any);
        let id = fb.new_local(recv_ty);
        fb.mf.params.push(id);
        let this_sym = interner.intern("this");
        scope.push((this_sym, id));
    }

    for (pi, p) in f.params.iter().enumerate() {
        let ty = typed
            .and_then(|t| {
                t.param_tys
                    .get(pi + if f.receiver_ty.is_some() { 1 } else { 0 })
                    .cloned()
            })
            .or_else(|| skotch_types::ty_from_name(interner.resolve(p.ty.name)))
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
            None, // no loop context at function body level
        ) {
            ok = false;
            break;
        }
    }
    // The current block's terminator stays `Return` (set by the
    // FnBuilder constructor and never overwritten for the last block).

    if ok {
        // Preserve param metadata from Pass 1.
        let saved_defaults = module.functions[fn_idx].param_defaults.clone();
        let saved_required = module.functions[fn_idx].required_params;
        let saved_names = module.functions[fn_idx].param_names.clone();
        module.functions[fn_idx] = fb.finish();
        module.functions[fn_idx].param_defaults = saved_defaults;
        module.functions[fn_idx].required_params = saved_required;
        module.functions[fn_idx].param_names = saved_names;
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
            required_params: 0,
            param_names: Vec::new(),
            param_defaults: Vec::new(),
            is_abstract: false,
        };
    }
}

/// Loop context for `break` and `continue`: (continue_target, break_target).
type LoopCtx = Option<(u32, u32)>;

#[allow(clippy::too_many_arguments)]
fn lower_stmt(
    stmt: &Stmt,
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    loop_ctx: LoopCtx,
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
            loop_ctx,
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
                if let Some(local) = lower_expr(
                    v,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                ) {
                    fb.set_terminator(Terminator::ReturnValue(local));
                }
            }
            true
        }
        Stmt::While { cond, body, .. } => {
            // while (cond) { body }
            //   → current → goto cond_block
            //     cond_block: eval cond → Branch(cond, body_block, exit_block)
            //     body_block: eval body → goto cond_block
            //     exit_block: continue
            let cond_block = fb.new_block();
            let body_block = fb.new_block();
            let exit_block = fb.new_block();
            fb.terminate_and_switch(Terminator::Goto(cond_block), cond_block);
            if let Some(cond_local) = lower_expr(
                cond,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) {
                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: cond_local,
                        then_block: body_block,
                        else_block: exit_block,
                    },
                    body_block,
                );
            }
            let lctx = Some((cond_block, exit_block));
            for s in &body.stmts {
                lower_stmt(
                    s,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    lctx,
                );
            }
            fb.terminate_and_switch(Terminator::Goto(cond_block), exit_block);
            true
        }
        Stmt::DoWhile { body, cond, .. } => {
            // do { body } while (cond)
            //   → body_block: body → cond_block: eval cond → Branch(cond, body_block, exit_block)
            let body_block = fb.new_block();
            let cond_block = fb.new_block();
            let exit_block = fb.new_block();
            fb.terminate_and_switch(Terminator::Goto(body_block), body_block);
            let lctx = Some((cond_block, exit_block));
            for s in &body.stmts {
                lower_stmt(
                    s,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    lctx,
                );
            }
            fb.terminate_and_switch(Terminator::Goto(cond_block), cond_block);
            if let Some(cond_local) = lower_expr(
                cond,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) {
                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: cond_local,
                        then_block: body_block,
                        else_block: exit_block,
                    },
                    exit_block,
                );
            }
            true
        }
        Stmt::Assign { target, value, .. } => {
            // var reassignment: look up the existing local, lower the
            // value, and assign to the same local ID.
            if let Some(rhs) = lower_expr(
                value,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) {
                // Find the local for this variable in scope.
                if let Some((_name, local_id)) =
                    scope.iter().rev().find(|(name, _)| *name == *target)
                {
                    let dest = *local_id;
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Local(rhs),
                    });
                }
            }
            true
        }
        Stmt::For {
            var_name,
            start: range_start,
            end: range_end,
            exclusive,
            descending,
            body,
            ..
        } => {
            // Desugar: for (i in a..b) { body }
            //   → var i = a
            //     val _end = b
            //     while (i <= _end) { body; i = i + 1 }
            let Some(start_val) = lower_expr(
                range_start,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) else {
                return false;
            };
            let Some(end_val) = lower_expr(
                range_end,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) else {
                return false;
            };

            // Create the loop variable.
            let loop_var = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest: loop_var,
                value: Rvalue::Local(start_val),
            });
            scope.push((*var_name, loop_var));

            // while (loop_var <= end_val)
            let cond_block = fb.new_block();
            let body_block = fb.new_block();
            let incr_block = fb.new_block(); // increment step (continue target)
            let exit_block = fb.new_block();

            fb.terminate_and_switch(Terminator::Goto(cond_block), cond_block);

            // Condition depends on range kind:
            // ..     → loop_var <= end_val
            // until  → loop_var <  end_val
            // downTo → loop_var >= end_val
            let cmp_op = if *descending {
                MBinOp::CmpGe
            } else if *exclusive {
                MBinOp::CmpLt
            } else {
                MBinOp::CmpLe
            };
            let cmp = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: cmp,
                value: Rvalue::BinOp {
                    op: cmp_op,
                    lhs: loop_var,
                    rhs: end_val,
                },
            });
            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: cmp,
                    then_block: body_block,
                    else_block: exit_block,
                },
                body_block,
            );

            // Body — continue goes to incr_block, break goes to exit_block
            let lctx = Some((incr_block, exit_block));
            for s in &body.stmts {
                lower_stmt(
                    s,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    lctx,
                );
            }

            // After body: goto increment block
            fb.terminate_and_switch(Terminator::Goto(incr_block), incr_block);

            // Step block: i = i + 1 (ascending) or i = i - 1 (descending)
            let step_op = if *descending {
                MBinOp::SubI
            } else {
                MBinOp::AddI
            };
            let one = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest: one,
                value: Rvalue::Const(MirConst::Int(1)),
            });
            let incremented = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest: incremented,
                value: Rvalue::BinOp {
                    op: step_op,
                    lhs: loop_var,
                    rhs: one,
                },
            });
            fb.push_stmt(MStmt::Assign {
                dest: loop_var,
                value: Rvalue::Local(incremented),
            });

            fb.terminate_and_switch(Terminator::Goto(cond_block), exit_block);
            true
        }
        Stmt::LocalFun(f) => {
            // Lower local function as a synthetic top-level function.
            let fn_idx = module.functions.len();
            let fn_name = interner.resolve(f.name).to_string();
            let return_ty = f
                .return_ty
                .as_ref()
                .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
                .unwrap_or(Ty::Unit);
            module.functions.push(MirFunction {
                id: FuncId(fn_idx as u32),
                name: fn_name,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty: return_ty.clone(),
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: false,
            });
            name_to_func.insert(f.name, FuncId(fn_idx as u32));

            // Lower the function body.
            lower_function(
                f,
                fn_idx,
                None,
                name_to_func,
                name_to_global,
                module,
                interner,
                diags,
            );
            true
        }
        Stmt::Break(_) => {
            if let Some((_continue_blk, break_blk)) = loop_ctx {
                fb.set_terminator(Terminator::Goto(break_blk));
            }
            true
        }
        Stmt::Continue(_) => {
            if let Some((continue_blk, _break_blk)) = loop_ctx {
                fb.set_terminator(Terminator::Goto(continue_blk));
            }
            true
        }
        Stmt::TryStmt {
            body, finally_body, ..
        } => {
            // Simplified: execute body, then finally (no catch/exception tables yet).
            for s in &body.stmts {
                let terminated = lower_stmt(
                    s,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                );
                if terminated {
                    break;
                }
            }
            if let Some(fb_block) = finally_body {
                for s in &fb_block.stmts {
                    let terminated = lower_stmt(
                        s,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    );
                    if terminated {
                        break;
                    }
                }
            }
            true
        }
        Stmt::ThrowStmt { .. } => {
            // throw is not yet compiled to athrow — skip but don't fail.
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
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
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
        None,
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
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    loop_ctx: LoopCtx,
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
        Expr::LongLit(v, _) => {
            let dest = fb.new_local(Ty::Long);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Long(*v)),
            });
            Some(dest)
        }
        Expr::DoubleLit(v, _) => {
            let dest = fb.new_local(Ty::Double);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Double(*v)),
            });
            Some(dest)
        }
        Expr::NullLit(_) => {
            let dest = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Null),
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
            loop_ctx,
        ),
        Expr::Binary {
            op,
            lhs,
            rhs,
            span: _,
        } => {
            // Short-circuit && and || — these lower to branches.
            if matches!(op, BinOp::And | BinOp::Or) {
                let result = fb.new_local(Ty::Bool);
                let rhs_block = fb.new_block();
                let merge_block = fb.new_block();
                let l = lower_expr(
                    lhs,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )?;
                if *op == BinOp::And {
                    // lhs && rhs: if lhs is false, result = false; else eval rhs
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Const(MirConst::Bool(false)),
                    });
                    fb.terminate_and_switch(
                        Terminator::Branch {
                            cond: l,
                            then_block: rhs_block,
                            else_block: merge_block,
                        },
                        rhs_block,
                    );
                } else {
                    // lhs || rhs: if lhs is true, result = true; else eval rhs
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Const(MirConst::Bool(true)),
                    });
                    fb.terminate_and_switch(
                        Terminator::Branch {
                            cond: l,
                            then_block: merge_block,
                            else_block: rhs_block,
                        },
                        rhs_block,
                    );
                }
                let r = lower_expr(
                    rhs,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )?;
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Local(r),
                });
                fb.terminate_and_switch(Terminator::Goto(merge_block), merge_block);
                return Some(result);
            }

            let l = lower_expr(
                lhs,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
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
                loop_ctx,
            )?;
            let lhs_ty = &fb.mf.locals[l.0 as usize];
            let rhs_ty = &fb.mf.locals[r.0 as usize];
            let is_double = matches!(lhs_ty, Ty::Double) || matches!(rhs_ty, Ty::Double);
            let is_long = matches!(lhs_ty, Ty::Long) || matches!(rhs_ty, Ty::Long);
            let (mop, result_ty) = match op {
                BinOp::Add if matches!(lhs_ty, Ty::String) => (MBinOp::ConcatStr, Ty::String),
                BinOp::Add if is_double => (MBinOp::AddD, Ty::Double),
                BinOp::Sub if is_double => (MBinOp::SubD, Ty::Double),
                BinOp::Mul if is_double => (MBinOp::MulD, Ty::Double),
                BinOp::Div if is_double => (MBinOp::DivD, Ty::Double),
                BinOp::Mod if is_double => (MBinOp::ModD, Ty::Double),
                BinOp::Add if is_long => (MBinOp::AddL, Ty::Long),
                BinOp::Sub if is_long => (MBinOp::SubL, Ty::Long),
                BinOp::Mul if is_long => (MBinOp::MulL, Ty::Long),
                BinOp::Div if is_long => (MBinOp::DivL, Ty::Long),
                BinOp::Mod if is_long => (MBinOp::ModL, Ty::Long),
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
                BinOp::And | BinOp::Or => unreachable!("handled above"),
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
                loop_ctx,
            )?;

            let then_blk = fb.new_block();
            let else_blk = fb.new_block();
            let merge_blk = fb.new_block();

            // The result local's type is inferred from the then-branch
            // after lowering it. We use a placeholder here that gets
            // replaced once the then-branch produces a value.
            // For now, start with Ty::Int as default (overridden below).
            let result = fb.new_local(Ty::Int); // type may be patched

            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: cond_local,
                    then_block: then_blk,
                    else_block: else_blk,
                },
                then_blk,
            );

            // Then branch.
            let mut then_val: Option<LocalId> = None;
            let mut then_terminates = false;
            for s in &then_block.stmts {
                match s {
                    skotch_syntax::Stmt::Expr(e) => {
                        then_val = lower_expr(
                            e,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            loop_ctx,
                        );
                    }
                    skotch_syntax::Stmt::Return { .. } => {
                        let _ = lower_stmt(
                            s,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            loop_ctx,
                        );
                        then_terminates = true;
                    }
                    skotch_syntax::Stmt::Break(_) | skotch_syntax::Stmt::Continue(_) => {
                        let _ = lower_stmt(
                            s,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            loop_ctx,
                        );
                        then_terminates = true;
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
                            loop_ctx,
                        );
                    }
                }
            }
            // Patch the result local's type to match the then-branch.
            if let Some(val) = then_val {
                let inferred_ty = fb.mf.locals[val.0 as usize].clone();
                fb.mf.locals[result.0 as usize] = inferred_ty;
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Local(val),
                });
            }
            // Only emit Goto(merge) if the then-block didn't contain
            // an explicit return statement.
            if then_terminates {
                fb.cur_block = else_blk;
            } else {
                fb.terminate_and_switch(Terminator::Goto(merge_blk), else_blk);
            }

            // Else branch.
            let mut else_terminates = false;
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
                                loop_ctx,
                            ) {
                                fb.push_stmt(MStmt::Assign {
                                    dest: result,
                                    value: Rvalue::Local(val),
                                });
                            }
                        }
                        skotch_syntax::Stmt::Return { .. }
                        | skotch_syntax::Stmt::Break(_)
                        | skotch_syntax::Stmt::Continue(_) => {
                            let _ = lower_stmt(
                                s,
                                fb,
                                scope,
                                module,
                                name_to_func,
                                name_to_global,
                                interner,
                                diags,
                                loop_ctx,
                            );
                            else_terminates = true;
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
                                loop_ctx,
                            );
                        }
                    }
                }
            }
            if else_terminates {
                fb.cur_block = merge_blk;
            } else {
                fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);
            }

            Some(result)
        }
        Expr::Call { callee, args, span } => {
            // Handle method calls on a receiver: `receiver.method(args)`
            if let Expr::Field { receiver, name, .. } = callee.as_ref() {
                let method_name = *name;

                // Check if this is an object method call (Singleton.method()).
                // Object methods are registered as top-level functions.
                if let Some(&fid) = name_to_func.get(&method_name) {
                    let ret_ty = module.functions[fid.0 as usize].return_ty.clone();
                    let mut arg_locals = Vec::new();
                    for a in args {
                        let id = lower_expr(
                            &a.expr,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            loop_ctx,
                        )?;
                        arg_locals.push(id);
                    }
                    let dest = fb.new_local(ret_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Static(fid),
                            args: arg_locals,
                        },
                    });
                    return Some(dest);
                }

                // Check if the receiver is a known Java class name for static calls.
                if let Some(static_call) = try_java_static_call(
                    receiver,
                    method_name,
                    args,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                ) {
                    return Some(static_call);
                }

                // If try_java_static_call returned None, check if the receiver
                // looks like an unresolvable external class and give a clear error.
                if let Some(qname) = extract_qualified_name(receiver, interner) {
                    if qname.starts_with(|c: char| c.is_uppercase()) || qname.contains('.') {
                        let method_str = interner.resolve(method_name);
                        diags.push(Diagnostic::error(
                            *span,
                            format!(
                                "unresolved reference: `{qname}.{method_str}` — class `{qname}` not found on the classpath"
                            ),
                        ));
                        return None;
                    }
                }

                // Lower the receiver as the first argument (instance method or extension).
                let recv_local = lower_expr(
                    receiver,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )?;
                let mut all_args = vec![recv_local];
                for a in args {
                    let id = lower_expr(
                        &a.expr,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )?;
                    all_args.push(id);
                }

                // Check if receiver is a class instance for virtual dispatch.
                let recv_ty = fb.mf.locals[recv_local.0 as usize].clone();
                let method_name_str = interner.resolve(method_name).to_string();

                // Override table for methods with ambiguous JVM overloads.
                // These need explicit descriptors because the JVM class has
                // multiple overloads that can't be distinguished by arg count.
                // Disambiguation table for JVM methods with multiple overloads
                // that share the same argument count. These cases can't be
                // resolved by class-file lookup alone without full type
                // inference on the argument expressions.
                let overload_override: Option<(&str, &str, &str, Ty)> =
                    match (&recv_ty, method_name_str.as_str(), args.len()) {
                        // String methods with CharSequence vs char overloads
                        (Ty::String, "replace", 2) => Some((
                            "java/lang/String",
                            "replace",
                            "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;",
                            Ty::String,
                        )),
                        (Ty::String, "contains", 1) => Some((
                            "java/lang/String",
                            "contains",
                            "(Ljava/lang/CharSequence;)Z",
                            Ty::Bool,
                        )),
                        (Ty::String, "indexOf", 1) => Some((
                            "java/lang/String",
                            "indexOf",
                            "(Ljava/lang/String;)I",
                            Ty::Int,
                        )),
                        (Ty::String, "lastIndexOf", 1) => Some((
                            "java/lang/String",
                            "lastIndexOf",
                            "(Ljava/lang/String;)I",
                            Ty::Int,
                        )),
                        (Ty::String, "startsWith", 1) => Some((
                            "java/lang/String",
                            "startsWith",
                            "(Ljava/lang/String;)Z",
                            Ty::Bool,
                        )),
                        (Ty::String, "endsWith", 1) => Some((
                            "java/lang/String",
                            "endsWith",
                            "(Ljava/lang/String;)Z",
                            Ty::Bool,
                        )),
                        (Ty::String, "repeat", 1) => Some((
                            "java/lang/String",
                            "repeat",
                            "(I)Ljava/lang/String;",
                            Ty::String,
                        )),
                        _ => None,
                    };
                if let Some((jvm_class, jvm_method, descriptor, ret_ty)) = overload_override {
                    let is_instance =
                        !matches!(&recv_ty, Ty::Int | Ty::Long | Ty::Double | Ty::Bool);
                    let dest = fb.new_local(ret_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: if is_instance {
                                CallKind::VirtualJava {
                                    class_name: jvm_class.to_string(),
                                    method_name: jvm_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                }
                            } else {
                                CallKind::StaticJava {
                                    class_name: jvm_class.to_string(),
                                    method_name: jvm_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                }
                            },
                            args: all_args,
                        },
                    });
                    return Some(dest);
                }

                // Resolve methods dynamically from JDK class files.
                let jvm_class_for_ty = match &recv_ty {
                    Ty::String => Some("java/lang/String"),
                    Ty::Int => Some("java/lang/Integer"),
                    Ty::Long => Some("java/lang/Long"),
                    Ty::Double => Some("java/lang/Double"),
                    Ty::Bool => Some("java/lang/Boolean"),
                    _ => None,
                };

                // Kotlin extension functions map to JVM method names.
                // These are stable ABI translations documented by JetBrains.
                let jvm_method_name = match (method_name_str.as_str(), jvm_class_for_ty) {
                    ("uppercase", Some("java/lang/String")) => "toUpperCase",
                    ("lowercase", Some("java/lang/String")) => "toLowerCase",
                    ("get", Some("java/lang/String")) => "charAt",
                    ("toInt", Some("java/lang/String")) => "parseInt__on__java/lang/Integer",
                    ("toDouble", Some("java/lang/String")) => "parseDouble__on__java/lang/Double",
                    ("toLong", Some("java/lang/String")) => "parseLong__on__java/lang/Long",
                    _ => method_name_str.as_str(),
                };

                // Handle cross-class redirections (e.g., String.toInt() → Integer.parseInt)
                let (effective_class, effective_method) =
                    if let Some(pos) = jvm_method_name.find("__on__") {
                        let method = &jvm_method_name[..pos];
                        let class = &jvm_method_name[pos + 6..];
                        (Some(class), method)
                    } else {
                        (jvm_class_for_ty, jvm_method_name)
                    };

                if let Some(jvm_class) = effective_class {
                    let is_primitive_ty =
                        matches!(&recv_ty, Ty::Int | Ty::Long | Ty::Double | Ty::Bool);
                    let is_cross_class = jvm_method_name.contains("__on__");

                    // For instance methods on reference types, try instance lookup first.
                    if !is_primitive_ty && !is_cross_class {
                        let instance_arg_count = args.len(); // don't count receiver
                                                             // Try the JVM-renamed name first.
                        if let Some((found_class, found_method, descriptor, ret_ty)) =
                            lookup_java_instance(jvm_class, effective_method, instance_arg_count)
                        {
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: found_class,
                                        method_name: found_method,
                                        descriptor,
                                    },
                                    args: all_args,
                                },
                            });
                            return Some(dest);
                        }
                        // Try original Kotlin name.
                        if effective_method != method_name_str.as_str() {
                            if let Some((found_class, found_method, descriptor, ret_ty)) =
                                lookup_java_instance(
                                    jvm_class,
                                    &method_name_str,
                                    instance_arg_count,
                                )
                            {
                                let dest = fb.new_local(ret_ty);
                                fb.push_stmt(MStmt::Assign {
                                    dest,
                                    value: Rvalue::Call {
                                        kind: CallKind::VirtualJava {
                                            class_name: found_class,
                                            method_name: found_method,
                                            descriptor,
                                        },
                                        args: all_args,
                                    },
                                });
                                return Some(dest);
                            }
                        }
                    }

                    // For static methods (primitive types, cross-class redirections).
                    let static_arg_count = args.len() + 1; // count receiver as arg
                    if let Some((found_class, found_method, descriptor, ret_ty)) =
                        lookup_java_static(jvm_class, effective_method, static_arg_count)
                    {
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::StaticJava {
                                    class_name: found_class,
                                    method_name: found_method,
                                    descriptor,
                                },
                                args: all_args,
                            },
                        });
                        return Some(dest);
                    }
                }

                let (kind, dest_ty) = if let Ty::Class(class_name) = &recv_ty {
                    // Look up method in class, walking the superclass chain.
                    let mut return_ty = Ty::Unit;
                    let mut search = Some(class_name.clone());
                    while let Some(ref cname) = search {
                        if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                            if let Some(m) = cls.methods.iter().find(|m| m.name == method_name_str)
                            {
                                return_ty = m.return_ty.clone();
                                break;
                            }
                            search = cls.super_class.clone();
                        } else {
                            break;
                        }
                    }
                    (
                        CallKind::Virtual {
                            class_name: class_name.clone(),
                            method_name: method_name_str,
                        },
                        return_ty,
                    )
                } else if let Some(fid) = name_to_func.get(&method_name) {
                    (
                        CallKind::Static(*fid),
                        module.functions[fid.0 as usize].return_ty.clone(),
                    )
                } else {
                    diags.push(Diagnostic::error(
                        *span,
                        format!("unknown method `{}`", interner.resolve(method_name)),
                    ));
                    return None;
                };
                let dest = fb.new_local(dest_ty);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind,
                        args: all_args,
                    },
                });
                return Some(dest);
            }

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

            // ─── Check for constructor call (class instantiation) ────
            let callee_str = interner.resolve(callee_name).to_string();
            let is_class = module.classes.iter().any(|c| c.name == callee_str);
            if is_class {
                // Lower as: NewInstance + Constructor call.
                let mut arg_locals = Vec::new();
                for a in args {
                    let id = lower_expr(
                        &a.expr,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )?;
                    arg_locals.push(id);
                }
                let dest = fb.new_local(Ty::Class(callee_str.clone()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance(callee_str.clone()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::Constructor(callee_str),
                        args: arg_locals,
                    },
                });
                return Some(dest);
            }

            // ─── Special form: println(<string template>) ───────────
            if interner.resolve(callee_name) == "println"
                && args.len() == 1
                && matches!(&args[0].expr, Expr::StringTemplate(_, _))
            {
                if let Expr::StringTemplate(parts, _) = &args[0].expr {
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

            // Lower arguments. If any are named, we'll reorder after.
            let has_named = args.iter().any(|a| a.name.is_some());
            let mut named_pairs: Vec<(Option<Symbol>, LocalId)> = Vec::new();
            for a in args {
                let id = lower_expr(
                    &a.expr,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )?;
                named_pairs.push((a.name, id));
            }

            // Reorder named arguments to match parameter order.
            let mut arg_locals: Vec<LocalId> = if has_named {
                // Look up the function's parameter names from the MIR module.
                let param_name_strs: Vec<String> = name_to_func
                    .get(&callee_name)
                    .map(|fid| module.functions[fid.0 as usize].param_names.clone())
                    .unwrap_or_default();

                if param_name_strs.is_empty() {
                    // No param names found — keep positional order.
                    named_pairs.iter().map(|(_, id)| *id).collect()
                } else {
                    let mut reordered = vec![None; param_name_strs.len()];
                    let mut positional_idx = 0;
                    for (name_opt, id) in &named_pairs {
                        if let Some(name_sym) = name_opt {
                            let name_str = interner.resolve(*name_sym);
                            if let Some(pos) = param_name_strs.iter().position(|pn| pn == name_str)
                            {
                                reordered[pos] = Some(*id);
                            } else {
                                // Unknown param name — keep in order.
                                if positional_idx < reordered.len() {
                                    reordered[positional_idx] = Some(*id);
                                    positional_idx += 1;
                                }
                            }
                        } else {
                            // Positional arg: fill the next unfilled slot.
                            while positional_idx < reordered.len()
                                && reordered[positional_idx].is_some()
                            {
                                positional_idx += 1;
                            }
                            if positional_idx < reordered.len() {
                                reordered[positional_idx] = Some(*id);
                                positional_idx += 1;
                            }
                        }
                    }
                    reordered.into_iter().flatten().collect()
                }
            } else {
                named_pairs.iter().map(|(_, id)| *id).collect()
            };

            // If fewer args provided than params, inject default values.
            if let Some(fid) = name_to_func.get(&callee_name) {
                let defaults = module.functions[fid.0 as usize].param_defaults.clone();
                let total_params = defaults.len();
                if !defaults.is_empty() && arg_locals.len() < total_params {
                    for i in arg_locals.len()..total_params {
                        if let Some(Some(default_const)) = defaults.get(i) {
                            let ty = const_ty(default_const);
                            let id = fb.new_local(ty);
                            fb.push_stmt(MStmt::Assign {
                                dest: id,
                                value: Rvalue::Const(default_const.clone()),
                            });
                            arg_locals.push(id);
                        }
                    }
                }
            }

            let callee_str = interner.resolve(callee_name).to_string();
            let callee_str = callee_str.as_str();

            // Handle stdlib top-level functions as StaticJava calls.
            let stdlib_call = match (callee_str, args.len()) {
                ("maxOf", 2) => Some(("java/lang/Math", "max", "(II)I", Ty::Int)),
                ("minOf", 2) => Some(("java/lang/Math", "min", "(II)I", Ty::Int)),
                _ => None,
            };
            if let Some((class, method, desc, ret_ty)) = stdlib_call {
                let dest = fb.new_local(ret_ty);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: class.to_string(),
                            method_name: method.to_string(),
                            descriptor: desc.to_string(),
                        },
                        args: arg_locals,
                    },
                });
                return Some(dest);
            }

            let kind = if callee_str == "println" {
                CallKind::Println
            } else if callee_str == "print" {
                CallKind::Print
            } else if let Some(fid) = name_to_func.get(&callee_name) {
                CallKind::Static(*fid)
            } else {
                // Try implicit `this.method()` — if `this` is in scope
                // and its class has a matching method, emit a virtual call.
                let this_sym = interner.intern("this");
                let this_info = scope.iter().rev().find(|(s, _)| *s == this_sym);
                let mut resolved = None;
                if let Some((_sym, this_local)) = this_info {
                    let this_ty = &fb.mf.locals[this_local.0 as usize];
                    if let Ty::Class(class_name) = this_ty {
                        // Look up in this class and superclasses.
                        let mut search_class = Some(class_name.clone());
                        while let Some(ref cname) = search_class {
                            let found = module.classes.iter().find(|c| &c.name == cname);
                            if let Some(cls) = found {
                                if cls.methods.iter().any(|m| m.name == callee_str) {
                                    // Prepend `this` as first arg.
                                    arg_locals.insert(0, *this_local);
                                    let ret_ty = cls
                                        .methods
                                        .iter()
                                        .find(|m| m.name == callee_str)
                                        .map(|m| m.return_ty.clone())
                                        .unwrap_or(Ty::Unit);
                                    resolved = Some((
                                        CallKind::Virtual {
                                            class_name: cname.clone(),
                                            method_name: callee_str.to_string(),
                                        },
                                        ret_ty,
                                    ));
                                    break;
                                }
                                search_class = cls.super_class.clone();
                            } else {
                                break;
                            }
                        }
                    }
                }
                if let Some((k, _)) = resolved {
                    k
                } else {
                    diags.push(Diagnostic::error(
                        *span,
                        format!("unknown call target `{}`", interner.resolve(callee_name)),
                    ));
                    return None;
                }
            };

            // Determine return type from the call kind.
            let dest_ty = match &kind {
                CallKind::Static(fid) => module.functions[fid.0 as usize].return_ty.clone(),
                CallKind::Virtual {
                    class_name,
                    method_name,
                } => {
                    let mut ret = Ty::Unit;
                    let mut search = Some(class_name.clone());
                    while let Some(ref cname) = search {
                        if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                            if let Some(m) = cls.methods.iter().find(|m| &m.name == method_name) {
                                ret = m.return_ty.clone();
                                break;
                            }
                            search = cls.super_class.clone();
                        } else {
                            break;
                        }
                    }
                    ret
                }
                _ => Ty::Unit,
            };

            // Autobox primitive args when the target parameter is Any.
            if let CallKind::Static(fid) = &kind {
                let target = &module.functions[fid.0 as usize];
                for (i, arg_id) in arg_locals.iter_mut().enumerate() {
                    if i < target.params.len() {
                        let param_ty = &target.locals[target.params[i].0 as usize];
                        let arg_ty = &fb.mf.locals[arg_id.0 as usize];
                        if matches!(param_ty, Ty::Any)
                            && !matches!(
                                arg_ty,
                                Ty::Any | Ty::String | Ty::Class(_) | Ty::Nullable(_)
                            )
                        {
                            let (box_class, box_desc) = match arg_ty {
                                Ty::Int => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
                                Ty::Bool => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
                                Ty::Long => ("java/lang/Long", "(J)Ljava/lang/Long;"),
                                Ty::Double => ("java/lang/Double", "(D)Ljava/lang/Double;"),
                                _ => continue,
                            };
                            let boxed = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: boxed,
                                value: Rvalue::Call {
                                    kind: CallKind::StaticJava {
                                        class_name: box_class.to_string(),
                                        method_name: "valueOf".to_string(),
                                        descriptor: box_desc.to_string(),
                                    },
                                    args: vec![*arg_id],
                                },
                            });
                            *arg_id = boxed;
                        }
                    }
                }
            }

            let dest = fb.new_local(dest_ty);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Call {
                    kind,
                    args: arg_locals,
                },
            });
            Some(dest)
        }
        Expr::StringTemplate(parts, _) => {
            // Lower string template to a chain of ConcatStr operations.
            // Start with the first part and concatenate the rest.
            let mut result: Option<LocalId> = None;
            for part in parts {
                let part_local = lower_template_part(
                    part,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                )?;
                result = Some(match result {
                    None => part_local,
                    Some(prev) => {
                        let concat = fb.new_local(Ty::String);
                        fb.push_stmt(MStmt::Assign {
                            dest: concat,
                            value: Rvalue::BinOp {
                                op: MBinOp::ConcatStr,
                                lhs: prev,
                                rhs: part_local,
                            },
                        });
                        concat
                    }
                });
            }
            result
        }
        Expr::Unary { op, operand, .. } => {
            // For negation of an integer literal, fold to a negative
            // constant directly. For general expressions, emit a sub.
            match op {
                skotch_syntax::UnaryOp::Neg => {
                    // Check for constant folding: -<intlit> or -<doublelit>
                    if let Expr::IntLit(v, _) = operand.as_ref() {
                        let dest = fb.new_local(Ty::Int);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Const(MirConst::Int(-(*v as i32))),
                        });
                        return Some(dest);
                    }
                    if let Expr::LongLit(v, _) = operand.as_ref() {
                        let dest = fb.new_local(Ty::Long);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Const(MirConst::Long(-*v)),
                        });
                        return Some(dest);
                    }
                    if let Expr::DoubleLit(v, _) = operand.as_ref() {
                        let dest = fb.new_local(Ty::Double);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Const(MirConst::Double(-*v)),
                        });
                        return Some(dest);
                    }
                    // General: 0 - operand
                    let val = lower_expr(
                        operand,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )?;
                    let val_ty = fb.mf.locals[val.0 as usize].clone();
                    if matches!(val_ty, Ty::Long) {
                        let zero = fb.new_local(Ty::Long);
                        fb.push_stmt(MStmt::Assign {
                            dest: zero,
                            value: Rvalue::Const(MirConst::Long(0)),
                        });
                        let dest = fb.new_local(Ty::Long);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::BinOp {
                                op: MBinOp::SubL,
                                lhs: zero,
                                rhs: val,
                            },
                        });
                        return Some(dest);
                    }
                    if matches!(val_ty, Ty::Double) {
                        let zero = fb.new_local(Ty::Double);
                        fb.push_stmt(MStmt::Assign {
                            dest: zero,
                            value: Rvalue::Const(MirConst::Double(0.0)),
                        });
                        let dest = fb.new_local(Ty::Double);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::BinOp {
                                op: MBinOp::SubD,
                                lhs: zero,
                                rhs: val,
                            },
                        });
                        return Some(dest);
                    }
                    let zero = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: zero,
                        value: Rvalue::Const(MirConst::Int(0)),
                    });
                    let dest = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::BinOp {
                            op: MBinOp::SubI,
                            lhs: zero,
                            rhs: val,
                        },
                    });
                    Some(dest)
                }
                skotch_syntax::UnaryOp::Not => {
                    // !bool → 1 - bool (since bools are 0/1 ints)
                    let val = lower_expr(
                        operand,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )?;
                    let one = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: one,
                        value: Rvalue::Const(MirConst::Int(1)),
                    });
                    let dest = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::BinOp {
                            op: MBinOp::SubI,
                            lhs: one,
                            rhs: val,
                        },
                    });
                    Some(dest)
                }
            }
        }
        Expr::When {
            subject,
            branches,
            else_body,
            ..
        } => {
            // Lower `when (subject) { v1 -> e1; v2 -> e2; else -> e3 }`
            // as a chain of if-else comparisons:
            //   val tmp = subject
            //   if (tmp == v1) e1
            //   else if (tmp == v2) e2
            //   else e3
            // Detect subjectless when: subject is BoolLit(true).
            let is_subjectless = matches!(subject.as_ref(), Expr::BoolLit(true, _));
            let subj = lower_expr(
                subject,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;

            let result = fb.new_local(Ty::String); // type patched below

            // Pre-allocate ALL blocks so they're in the right order:
            // [cmp1_blk, body1_blk, cmp2_blk, body2_blk, ..., else_blk, merge_blk]
            // This ensures merge_blk has the highest index and is emitted last.
            let mut cmp_blks = Vec::new();
            let mut body_blks = Vec::new();
            for _ in branches {
                cmp_blks.push(fb.new_block());
                body_blks.push(fb.new_block());
            }
            let else_blk = if else_body.is_some() {
                fb.new_block()
            } else {
                0 // unused
            };
            let merge_blk = fb.new_block();

            // First comparison block
            if !branches.is_empty() {
                fb.terminate_and_switch(Terminator::Goto(cmp_blks[0]), cmp_blks[0]);
            } else if else_body.is_some() {
                fb.terminate_and_switch(Terminator::Goto(else_blk), else_blk);
            }

            for (i, branch) in branches.iter().enumerate() {
                // We're now in cmp_blks[i]
                let pattern_local = lower_expr(
                    &branch.pattern,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )?;

                // Determine the comparison for this branch.
                let cmp = if let Some(ref range_end_expr) = branch.range_end {
                    // Range pattern: subject >= start && subject <= end
                    let range_end_local = lower_expr(
                        range_end_expr,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )?;
                    let ge = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest: ge,
                        value: Rvalue::BinOp {
                            op: MBinOp::CmpGe,
                            lhs: subj,
                            rhs: pattern_local,
                        },
                    });
                    let le = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest: le,
                        value: Rvalue::BinOp {
                            op: MBinOp::CmpLe,
                            lhs: subj,
                            rhs: range_end_local,
                        },
                    });
                    // AND them together using short-circuit
                    // For simplicity, use a non-short-circuit AND:
                    // result = ge * le (both are 0/1 ints)
                    let both = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest: both,
                        value: Rvalue::BinOp {
                            op: MBinOp::MulI,
                            lhs: ge,
                            rhs: le,
                        },
                    });
                    both
                } else if is_subjectless {
                    // Subjectless when: the pattern IS the boolean condition.
                    pattern_local
                } else {
                    // Subject when: compare subject == pattern.
                    let c = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::BinOp {
                            op: MBinOp::CmpEq,
                            lhs: subj,
                            rhs: pattern_local,
                        },
                    });
                    c
                };

                let fall_through = if i + 1 < branches.len() {
                    cmp_blks[i + 1]
                } else if else_body.is_some() {
                    else_blk
                } else {
                    merge_blk
                };

                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: cmp,
                        then_block: body_blks[i],
                        else_block: fall_through,
                    },
                    body_blks[i],
                );

                // Lower body in body_blks[i]
                if let Some(val) = lower_expr(
                    &branch.body,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                ) {
                    if i == 0 {
                        let ty = fb.mf.locals[val.0 as usize].clone();
                        fb.mf.locals[result.0 as usize] = ty;
                    }
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(val),
                    });
                }
                // Goto merge, switch to next comparison block
                let next = if i + 1 < branches.len() {
                    cmp_blks[i + 1]
                } else if else_body.is_some() {
                    else_blk
                } else {
                    merge_blk
                };
                fb.terminate_and_switch(Terminator::Goto(merge_blk), next);
            }

            // Else body
            if let Some(eb) = else_body {
                // We're in else_blk
                if let Some(val) = lower_expr(
                    eb,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                ) {
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(val),
                    });
                }
                fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);
            } else {
                fb.cur_block = merge_blk;
            }

            Some(result)
        }
        Expr::Field {
            receiver,
            name,
            span,
        } => {
            // Check if this is an enum/object constant access (Color.RED).
            // If the field name is a known zero-arg function, call it.
            if let Some(&fid) = name_to_func.get(name) {
                let ret_ty = module.functions[fid.0 as usize].return_ty.clone();
                let params_len = module.functions[fid.0 as usize].params.len();
                if params_len == 0 {
                    let dest = fb.new_local(ret_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Static(fid),
                            args: vec![],
                        },
                    });
                    return Some(dest);
                }
            }

            // Try to lower as a field access on a class instance.
            if let Some(recv_local) = lower_expr(
                receiver,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) {
                let recv_ty = fb.mf.locals[recv_local.0 as usize].clone();
                if let Ty::Class(class_name) = recv_ty {
                    let field_name = interner.resolve(*name).to_string();
                    let field_ty = module
                        .classes
                        .iter()
                        .find(|c| c.name == class_name)
                        .and_then(|c| c.fields.iter().find(|f| f.name == field_name))
                        .map(|f| f.ty.clone())
                        .unwrap_or(Ty::Any);
                    let dest = fb.new_local(field_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::GetField {
                            receiver: recv_local,
                            class_name,
                            field_name,
                        },
                    });
                    return Some(dest);
                }
                // Handle built-in type properties.
                let field_name = interner.resolve(*name).to_string();
                if matches!(recv_ty, Ty::String) && field_name == "length" {
                    // String.length → invokevirtual String.length()I
                    let dest = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Virtual {
                                class_name: "java/lang/String".to_string(),
                                method_name: "length".to_string(),
                            },
                            args: vec![recv_local],
                        },
                    });
                    return Some(dest);
                }
            }
            // Not a class field access — could be a Java package path.
            // Return None to let the caller handle it.
            let _ = (name_to_func, *span);
            None
        }
        Expr::ElvisOp { lhs, rhs, .. } => {
            // lhs ?: rhs → if (lhs != null) lhs else rhs
            let l = lower_expr(
                lhs,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            let then_block = fb.new_block();
            let else_block = fb.new_block();
            let merge_block = fb.new_block();

            // null check: compare lhs to null
            let null_local = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
            fb.push_stmt(MStmt::Assign {
                dest: null_local,
                value: Rvalue::Const(MirConst::Null),
            });
            let cmp = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: cmp,
                value: Rvalue::BinOp {
                    op: MBinOp::CmpNe,
                    lhs: l,
                    rhs: null_local,
                },
            });
            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: cmp,
                    then_block,
                    else_block,
                },
                then_block,
            );

            // Result typed as Any (java/lang/Object) so both branches
            // (nullable lhs or concrete rhs) are verifier-compatible.
            let result = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Local(l),
            });
            fb.terminate_and_switch(Terminator::Goto(merge_block), else_block);

            // else: result = rhs
            let r = lower_expr(
                rhs,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Local(r),
            });
            fb.terminate_and_switch(Terminator::Goto(merge_block), merge_block);

            Some(result)
        }
        Expr::Throw { expr: thrown, .. } => {
            // For now, throw is lowered as println("Exception: ...") + return.
            // Real throw would need athrow opcode support.
            // We emit a simple diagnostic for now.
            let _ = (
                thrown,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
            );
            diags.push(Diagnostic::error(
                thrown.span(),
                "throw expressions are not yet fully compiled (exception tables needed)",
            ));
            None
        }
        Expr::Try {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            // Simplified try: just execute the body.
            // Catch/finally are parsed but not compiled (need exception tables).
            for stmt in &body.stmts {
                let terminated = lower_stmt(
                    stmt,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                );
                if terminated {
                    break;
                }
            }
            // Execute finally body if present (unconditionally).
            if let Some(fb_block) = finally_body {
                for stmt in &fb_block.stmts {
                    let terminated = lower_stmt(
                        stmt,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    );
                    if terminated {
                        break;
                    }
                }
            }
            let _ = catch_body; // catch needs exception tables
            None
        }
        Expr::SafeCall { receiver, name, .. } => {
            // receiver?.name → if (receiver != null) receiver.name else null
            let recv = lower_expr(
                receiver,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            let then_block = fb.new_block();
            let else_block = fb.new_block();
            let merge_block = fb.new_block();

            let null_local = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
            fb.push_stmt(MStmt::Assign {
                dest: null_local,
                value: Rvalue::Const(MirConst::Null),
            });
            let cmp = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: cmp,
                value: Rvalue::BinOp {
                    op: MBinOp::CmpNe,
                    lhs: recv,
                    rhs: null_local,
                },
            });
            let result = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: cmp,
                    then_block,
                    else_block,
                },
                then_block,
            );

            // then: result = receiver.name (field access)
            let field_name = interner.resolve(*name).to_string();
            let field_val = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: field_val,
                value: Rvalue::GetField {
                    receiver: recv,
                    class_name: String::new(), // resolved at codegen
                    field_name,
                },
            });
            fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Local(field_val),
            });
            fb.terminate_and_switch(Terminator::Goto(merge_block), else_block);

            // else: result = null
            fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Const(MirConst::Null),
            });
            fb.terminate_and_switch(Terminator::Goto(merge_block), merge_block);

            Some(result)
        }
        Expr::IsCheck {
            expr: checked,
            type_name,
            negated,
            ..
        } => {
            let obj = lower_expr(
                checked,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            let type_str = interner.resolve(*type_name);
            // Map Kotlin type names to JVM internal names for instanceof.
            let jvm_type = match type_str {
                "String" => "java/lang/String",
                "Int" => "java/lang/Integer",
                "Long" => "java/lang/Long",
                "Double" => "java/lang/Double",
                "Boolean" => "java/lang/Boolean",
                "Any" => "java/lang/Object",
                other => {
                    // User-defined class — use as-is (single-segment name).
                    // Leak into a stable &str for the descriptor.
                    // (For now, assume same-module classes.)
                    other
                }
            };
            let dest = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::InstanceOf {
                    obj,
                    type_descriptor: jvm_type.to_string(),
                },
            });
            if *negated {
                // `!is` — negate the result: dest_neg = (dest == 0)
                let zero = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: zero,
                    value: Rvalue::Const(MirConst::Bool(false)),
                });
                let neg = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: neg,
                    value: Rvalue::BinOp {
                        op: MBinOp::CmpEq,
                        lhs: dest,
                        rhs: zero,
                    },
                });
                Some(neg)
            } else {
                Some(dest)
            }
            // TODO: smart casts — narrow obj's type in the then-branch
        }
        Expr::AsCast { expr: casted, .. } => {
            // For now, `as` is a no-op — just return the expression.
            lower_expr(
                casted,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )
        }
        Expr::NotNullAssert { expr: asserted, .. } => {
            // `!!` is a no-op at MIR level (would need null check + throw KotlinNullPointerException).
            lower_expr(
                asserted,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )
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
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
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
                None,
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
            None,
        ),
    }
}

/// Look up a Java static method by class name and method name.
/// First tries the real JDK class file registry, then falls back to
/// the hardcoded table for environments without a JDK.
fn lookup_java_static(
    class_name: &str,
    method_name: &str,
    arg_count: usize,
) -> Option<(String, String, String, Ty)> {
    // If the name is already a JVM internal path (contains '/'), use it directly.
    if class_name.contains('/') {
        return lookup_java_static_from_jdk(class_name, method_name, arg_count);
    }
    // If it's a dot-qualified name like "java.lang.System", convert to JVM
    // path "java/lang/System" and look up by full path.
    if class_name.contains('.') {
        let jvm_path = class_name.replace('.', "/");
        return lookup_java_static_from_jdk(&jvm_path, method_name, arg_count);
    }
    // Simple name like "System" — look up in registry (which maps simple
    // names to pre-loaded classes from java.lang.* and kotlin.*).
    lookup_java_static_from_jdk(class_name, method_name, arg_count)
}

/// Look up an instance (non-static) method on a JVM class.
fn lookup_java_instance(
    class_name: &str,
    method_name: &str,
    arg_count: usize,
) -> Option<(String, String, String, Ty)> {
    if class_name.contains('/') {
        return lookup_java_instance_from_jdk(class_name, method_name, arg_count);
    }
    if class_name.contains('.') {
        let jvm_path = class_name.replace('.', "/");
        return lookup_java_instance_from_jdk(&jvm_path, method_name, arg_count);
    }
    let simple = class_name;
    lookup_java_instance_from_jdk(simple, method_name, arg_count)
}

/// Look up a static method from the JDK's actual class files.
/// Count parameters in a JVM method descriptor.
fn count_descriptor_params(desc: &str) -> usize {
    let inner = desc.split(')').next().unwrap_or("").trim_start_matches('(');
    let mut count = 0;
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' => count += 1,
            'L' => {
                // Skip to ';'
                for sc in chars.by_ref() {
                    if sc == ';' {
                        break;
                    }
                }
                count += 1;
            }
            '[' => {} // array prefix, don't count yet
            _ => {}
        }
    }
    count
}

fn lookup_java_static_from_jdk(
    class_name: &str,
    method_name: &str,
    arg_count: usize,
) -> Option<(String, String, String, Ty)> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static REGISTRY: Mutex<Option<HashMap<String, skotch_classinfo::ClassInfo>>> = Mutex::new(None);

    let mut guard = REGISTRY.lock().ok()?;
    let reg = guard.get_or_insert_with(skotch_classinfo::build_jdk_registry);

    // Try to find the class in the registry. If not found, try to load it.
    if !reg.contains_key(class_name) {
        // Map simple name → JVM path for common packages.
        let jvm_path = if class_name.contains('/') {
            class_name.to_string()
        } else {
            format!("java/lang/{class_name}")
        };
        if let Ok(info) = skotch_classinfo::load_jdk_class(&jvm_path) {
            reg.insert(class_name.to_string(), info);
        }
    }

    let class_info = reg.get(class_name)?;
    // First try: match by name AND parameter count.
    let method = class_info
        .methods
        .iter()
        .find(|m| {
            m.name == method_name
                && m.is_static()
                && m.is_public()
                && count_descriptor_params(&m.descriptor) == arg_count
        })
        // Fallback: just match by name.
        .or_else(|| {
            class_info
                .methods
                .iter()
                .find(|m| m.name == method_name && m.is_static() && m.is_public())
        })?;

    let return_ty = match skotch_classinfo::return_type_from_descriptor(&method.descriptor) {
        "Unit" => Ty::Unit,
        "Boolean" => Ty::Bool,
        "Int" => Ty::Int,
        "Long" => Ty::Long,
        "Double" => Ty::Double,
        "String" => Ty::String,
        _ => Ty::Any,
    };

    Some((
        class_info.name.clone(),
        method.name.clone(),
        method.descriptor.clone(),
        return_ty,
    ))
}

/// Like `lookup_java_static_from_jdk` but for instance methods (not static).
fn lookup_java_instance_from_jdk(
    class_name: &str,
    method_name: &str,
    arg_count: usize,
) -> Option<(String, String, String, Ty)> {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static REGISTRY: Mutex<Option<HashMap<String, skotch_classinfo::ClassInfo>>> = Mutex::new(None);

    let mut guard = REGISTRY.lock().ok()?;
    let reg = guard.get_or_insert_with(skotch_classinfo::build_jdk_registry);

    if !reg.contains_key(class_name) {
        let jvm_path = if class_name.contains('/') {
            class_name.to_string()
        } else {
            format!("java/lang/{class_name}")
        };
        if let Ok(info) = skotch_classinfo::load_jdk_class(&jvm_path) {
            reg.insert(class_name.to_string(), info);
        }
    }

    let class_info = reg.get(class_name)?;
    let method = class_info
        .methods
        .iter()
        .find(|m| {
            m.name == method_name
                && !m.is_static()
                && m.is_public()
                && count_descriptor_params(&m.descriptor) == arg_count
        })
        .or_else(|| {
            class_info
                .methods
                .iter()
                .find(|m| m.name == method_name && !m.is_static() && m.is_public())
        })?;

    let return_ty = match skotch_classinfo::return_type_from_descriptor(&method.descriptor) {
        "Unit" => Ty::Unit,
        "Boolean" => Ty::Bool,
        "Int" => Ty::Int,
        "Long" => Ty::Long,
        "Double" => Ty::Double,
        "String" => Ty::String,
        _ => Ty::Any,
    };

    Some((
        class_info.name.clone(),
        method.name.clone(),
        method.descriptor.clone(),
        return_ty,
    ))
}

/// Try to lower a method call as a Java static call. Returns Some(dest_local) if successful.
#[allow(clippy::too_many_arguments)]
fn try_java_static_call(
    receiver: &Expr,
    method_name: Symbol,
    args: &[skotch_syntax::CallArg],
    fb: &mut FnBuilder,
    scope: &mut Vec<(Symbol, LocalId)>,
    module: &mut MirModule,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    loop_ctx: LoopCtx,
) -> Option<LocalId> {
    // Extract the class name from the receiver expression.
    // Supports: `System` (simple), `java.lang.System` (qualified).
    let class_name = extract_qualified_name(receiver, interner)?;

    let method_str = interner.resolve(method_name).to_string();
    let (jvm_class, jvm_method, descriptor, return_ty) =
        lookup_java_static(&class_name, &method_str, args.len())?;

    // Lower arguments.
    let mut arg_locals = Vec::new();
    for a in args {
        let id = lower_expr(
            &a.expr,
            fb,
            scope,
            module,
            name_to_func,
            name_to_global,
            interner,
            diags,
            loop_ctx,
        )?;
        arg_locals.push(id);
    }

    let dest = fb.new_local(return_ty);
    fb.push_stmt(MStmt::Assign {
        dest,
        value: Rvalue::Call {
            kind: CallKind::StaticJava {
                class_name: jvm_class,
                method_name: jvm_method,
                descriptor,
            },
            args: arg_locals,
        },
    });
    Some(dest)
}

/// Extract a qualified name from a chain of Field expressions.
/// `java.lang.System` → "java.lang.System"
/// `System` → "System"
fn extract_qualified_name(expr: &Expr, interner: &Interner) -> Option<String> {
    match expr {
        Expr::Ident(sym, _) => Some(interner.resolve(*sym).to_string()),
        Expr::Field { receiver, name, .. } => {
            let prefix = extract_qualified_name(receiver, interner)?;
            let segment = interner.resolve(*name);
            Some(format!("{prefix}.{segment}"))
        }
        _ => None,
    }
}

/// Lower an `object` declaration to a MirClass with static-like methods.
/// The object compiles to a regular class with an empty constructor.
/// Methods are instance methods (the JVM INSTANCE field dispatches to them).
/// Lower an `enum class` to top-level constant functions.
/// Each enum entry becomes a function returning its name as a String.
/// `Color.RED` resolves to calling the `RED` function which returns `"RED"`.
/// `.name` returns the string; `.ordinal` returns the index.
fn lower_enum(
    e: &skotch_syntax::EnumDecl,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    module: &mut MirModule,
    interner: &mut Interner,
) {
    let enum_name = interner.resolve(e.name).to_string();

    // For each entry, create a function that returns the entry's name as a String.
    // This is a simplified model — real enums have ordinals and are instances of a
    // class, but for basic usage (when matching, println, .name) strings suffice.
    for (ordinal, &entry_sym) in e.entries.iter().enumerate() {
        let entry_name = interner.resolve(entry_sym).to_string();
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);

        // Register the entry name so Color.RED resolves.
        name_to_func.insert(entry_sym, fn_id);

        let sid = module.intern_string(&entry_name);
        let mut fb = FnBuilder::new(fn_idx, entry_name, Ty::String);
        let result = fb.new_local(Ty::String);
        fb.push_stmt(MStmt::Assign {
            dest: result,
            value: Rvalue::Const(MirConst::String(sid)),
        });
        fb.set_terminator(Terminator::ReturnValue(result));
        module.add_function(fb.finish());

        let _ = (ordinal, &enum_name); // used later for .ordinal support
    }
}

/// Calls like `Singleton.greet()` are resolved as static calls on the
/// wrapper class that delegate to the object's methods.
fn lower_object(
    o: &skotch_syntax::ObjectDecl,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let obj_name = interner.resolve(o.name).to_string();

    // Build an empty <init> constructor.
    let mut init_fn = MirFunction {
        id: FuncId(0),
        name: "<init>".to_string(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::Unit,
        required_params: 0,
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract: false,
    };
    let _this_id = init_fn.new_local(Ty::Class(obj_name.clone()));
    init_fn.params.push(LocalId(0));

    // Lower each method as a top-level static function on the wrapper class.
    // This way `Singleton.greet()` resolves via try_java_static_call or
    // the normal name_to_func lookup.
    for method in &o.methods {
        let method_name = interner.resolve(method.name).to_string();
        let return_ty = method
            .return_ty
            .as_ref()
            .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
            .unwrap_or(Ty::Unit);

        // Register as a top-level function so Singleton.method() resolves.
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        name_to_func.insert(method.name, fn_id);

        let mut fb = FnBuilder::new(fn_idx, method_name, return_ty);
        // Object methods have no `this` — they're effectively static.
        let mut scope: Vec<(Symbol, LocalId)> = Vec::new();
        for p in &method.params {
            let ty = skotch_types::ty_from_name(interner.resolve(p.ty.name)).unwrap_or(Ty::Any);
            let id = fb.new_local(ty);
            fb.mf.params.push(id);
            scope.push((p.name, id));
        }

        for s in &method.body.stmts {
            lower_stmt(
                s,
                &mut fb,
                &mut scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                None,
            );
        }

        module.add_function(fb.finish());
    }

    // We don't add the object as a MirClass — its methods are top-level
    // static functions on the wrapper class. This matches how kotlinc
    // compiles `object` declarations for the JVM.
}

fn lower_class(
    c: &skotch_syntax::ClassDecl,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let class_name = interner.resolve(c.name).to_string();

    // Collect fields from constructor params (val/var) and body properties.
    let mut fields = Vec::new();
    for p in &c.constructor_params {
        if p.is_val || p.is_var {
            let ty = skotch_types::ty_from_name(interner.resolve(p.ty.name)).unwrap_or(Ty::Any);
            fields.push(MirField {
                name: interner.resolve(p.name).to_string(),
                ty,
            });
        }
    }
    for prop in &c.properties {
        let ty = prop
            .ty
            .as_ref()
            .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
            .unwrap_or(Ty::Int);
        fields.push(MirField {
            name: interner.resolve(prop.name).to_string(),
            ty,
        });
    }

    // Build the <init> constructor.
    let mut init_fn = MirFunction {
        id: FuncId(0),
        name: "<init>".to_string(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::Unit,
        required_params: 0,
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract: false,
    };
    // Add 'this' as local 0.
    let this_id = init_fn.new_local(Ty::Class(class_name.clone()));
    init_fn.params.push(this_id);
    // Add constructor params and field assignments.
    for p in &c.constructor_params {
        if p.is_val || p.is_var {
            let ty = skotch_types::ty_from_name(interner.resolve(p.ty.name)).unwrap_or(Ty::Any);
            let param_id = init_fn.new_local(ty);
            init_fn.params.push(param_id);
            let field_name = interner.resolve(p.name).to_string();
            init_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: this_id, // dummy dest
                value: Rvalue::PutField {
                    receiver: this_id,
                    class_name: class_name.clone(),
                    field_name,
                    value: param_id,
                },
            });
        }
    }
    // Initialize body properties with default values.
    for prop in &c.properties {
        let (val, ty) = if let Some(init) = &prop.init {
            match init {
                Expr::IntLit(v, _) => (Some(MirConst::Int(*v as i32)), Ty::Int),
                Expr::LongLit(v, _) => (Some(MirConst::Long(*v)), Ty::Long),
                Expr::DoubleLit(v, _) => (Some(MirConst::Double(*v)), Ty::Double),
                Expr::BoolLit(v, _) => (Some(MirConst::Bool(*v)), Ty::Bool),
                Expr::StringLit(s, _) => {
                    let sid = module.intern_string(s);
                    (Some(MirConst::String(sid)), Ty::String)
                }
                _ => (None, Ty::Int),
            }
        } else {
            (None, Ty::Int)
        };
        if let Some(c_val) = val {
            let val_id = init_fn.new_local(ty);
            init_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: val_id,
                value: Rvalue::Const(c_val),
            });
            let field_name = interner.resolve(prop.name).to_string();
            init_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: this_id, // dummy
                value: Rvalue::PutField {
                    receiver: this_id,
                    class_name: class_name.clone(),
                    field_name,
                    value: val_id,
                },
            });
        }
    }

    // Lower init blocks — execute statements in the constructor.
    // We need a FnBuilder-like scope for the init block body. Since init_fn
    // is a raw MirFunction (not a FnBuilder), we create a temporary FnBuilder,
    // lower the init block stmts, then merge the resulting stmts back.
    if !c.init_blocks.is_empty() {
        let init_fn_idx = module.functions.len() + 1000; // temporary index
        let mut fb = FnBuilder::new(init_fn_idx, "<init>".to_string(), Ty::Unit);
        // Transfer existing locals and stmts from init_fn into the FnBuilder.
        fb.mf.locals = init_fn.locals.clone();
        fb.mf.params = init_fn.params.clone();
        fb.mf.blocks[0].stmts = init_fn.blocks[0].stmts.clone();

        let this_sym = interner.intern("this");
        let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_id)];
        // Add constructor param names to scope.
        let mut param_idx = 1usize; // skip 'this' at 0
        for p in &c.constructor_params {
            if p.is_val || p.is_var {
                if param_idx < fb.mf.params.len() {
                    let param_local = fb.mf.params[param_idx];
                    scope.push((p.name, param_local));
                }
                param_idx += 1;
            }
        }

        for init_block in &c.init_blocks {
            for s in &init_block.stmts {
                lower_stmt(
                    s,
                    &mut fb,
                    &mut scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    None,
                );
            }
        }

        // Transfer the results back to init_fn.
        init_fn.locals = fb.mf.locals;
        init_fn.blocks = fb.mf.blocks;
        // Ensure the last block terminates with Return.
        if let Some(last) = init_fn.blocks.last_mut() {
            last.terminator = Terminator::Return;
        }
    }

    let super_class = c
        .parent_class
        .as_ref()
        .map(|sc| interner.resolve(sc.name).to_string());

    // Pre-register the class with method stubs so that implicit
    // `this.method()` resolution works during method body lowering.
    let stub_methods: Vec<MirFunction> = c
        .methods
        .iter()
        .map(|m| {
            let mname = interner.resolve(m.name).to_string();
            let rty = m
                .return_ty
                .as_ref()
                .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
                .unwrap_or(Ty::Unit);
            MirFunction {
                id: FuncId(0),
                name: mname,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty: rty,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: m.is_abstract,
            }
        })
        .collect();
    let iface_names: Vec<String> = c
        .interfaces
        .iter()
        .map(|s| interner.resolve(*s).to_string())
        .collect();
    let class_idx = module.classes.len();
    module.classes.push(MirClass {
        name: class_name.clone(),
        super_class: super_class.clone(),
        is_open: c.is_open,
        is_abstract: c.is_abstract,
        is_interface: false,
        interfaces: iface_names.clone(),
        fields: fields.clone(),
        methods: stub_methods,
        constructor: init_fn.clone(),
    });

    // Lower methods.
    let mut mir_methods = Vec::new();
    for method in &c.methods {
        let method_name = interner.resolve(method.name).to_string();
        let return_ty = method
            .return_ty
            .as_ref()
            .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
            .unwrap_or(Ty::Unit);

        let fn_idx = module.functions.len() + mir_methods.len();
        let mut fb = FnBuilder::new(fn_idx, method_name.clone(), return_ty);

        // Add implicit `this` parameter.
        let this_local = fb.new_local(Ty::Class(class_name.clone()));
        fb.mf.params.push(this_local);
        let this_sym = interner.intern("this");
        let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];

        // Add explicit parameters.
        for p in &method.params {
            let ty = skotch_types::ty_from_name(interner.resolve(p.ty.name)).unwrap_or(Ty::Any);
            let id = fb.new_local(ty);
            fb.mf.params.push(id);
            scope.push((p.name, id));
        }

        // Load fields into locals so they're accessible by name in the method body.
        // Track field→local mapping for writeback after the body.
        let mut field_locals: Vec<(String, LocalId)> = Vec::new();
        for field in &fields {
            let field_sym = interner.intern(&field.name);
            let field_local = fb.new_local(field.ty.clone());
            fb.push_stmt(MStmt::Assign {
                dest: field_local,
                value: Rvalue::GetField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });
            scope.push((field_sym, field_local));
            field_locals.push((field.name.clone(), field_local));
        }

        for s in &method.body.stmts {
            lower_stmt(
                s,
                &mut fb,
                &mut scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                None,
            );
        }

        // Write back all field locals to the object. This ensures that
        // mutations like `count = count + 1` persist after the method returns.
        for (field_name, field_local) in &field_locals {
            fb.push_stmt(MStmt::Assign {
                dest: this_local, // dummy dest
                value: Rvalue::PutField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: field_name.clone(),
                    value: *field_local,
                },
            });
        }

        let mut finished = fb.finish();
        finished.is_abstract = method.is_abstract;
        mir_methods.push(finished);
    }

    // Lower companion object methods as top-level static functions.
    // This makes ClassName.staticMethod() work via name_to_func lookup.
    for method in &c.companion_methods {
        let method_name = interner.resolve(method.name).to_string();
        let return_ty = method
            .return_ty
            .as_ref()
            .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
            .unwrap_or(Ty::Unit);

        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        name_to_func.insert(method.name, fn_id);

        let mut fb = FnBuilder::new(fn_idx, method_name, return_ty);
        let mut scope: Vec<(Symbol, LocalId)> = Vec::new();
        for p in &method.params {
            let ty = skotch_types::ty_from_name(interner.resolve(p.ty.name)).unwrap_or(Ty::Any);
            let id = fb.new_local(ty);
            fb.mf.params.push(id);
            scope.push((p.name, id));
        }

        for s in &method.body.stmts {
            lower_stmt(
                s,
                &mut fb,
                &mut scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                None,
            );
        }

        module.add_function(fb.finish());
    }

    // Synthesize toString() for data classes.
    if c.is_data && !fields.is_empty() {
        let ts_idx = module.functions.len() + mir_methods.len();
        let mut ts_fb = FnBuilder::new(ts_idx, "toString".to_string(), Ty::String);
        let ts_this = ts_fb.new_local(Ty::Class(class_name.clone()));
        ts_fb.mf.params.push(ts_this);

        // Build the string: "ClassName(f1=v1, f2=v2)"
        // Start with "ClassName(" as a string constant.
        let prefix = format!("{}(", class_name);
        let prefix_sid = module.intern_string(&prefix);
        let result = ts_fb.new_local(Ty::String);
        ts_fb.push_stmt(MStmt::Assign {
            dest: result,
            value: Rvalue::Const(MirConst::String(prefix_sid)),
        });

        for (fi, field) in fields.iter().enumerate() {
            // For each field after the first, prepend ", "
            if fi > 0 {
                let comma_sid = module.intern_string(", ");
                let comma = ts_fb.new_local(Ty::String);
                ts_fb.push_stmt(MStmt::Assign {
                    dest: comma,
                    value: Rvalue::Const(MirConst::String(comma_sid)),
                });
                let cat = ts_fb.new_local(Ty::String);
                ts_fb.push_stmt(MStmt::Assign {
                    dest: cat,
                    value: Rvalue::BinOp {
                        op: MBinOp::ConcatStr,
                        lhs: result,
                        rhs: comma,
                    },
                });
                ts_fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Local(cat),
                });
            }

            // Append "fieldName="
            let label_sid = module.intern_string(&format!("{}=", field.name));
            let label = ts_fb.new_local(Ty::String);
            ts_fb.push_stmt(MStmt::Assign {
                dest: label,
                value: Rvalue::Const(MirConst::String(label_sid)),
            });
            let cat1 = ts_fb.new_local(Ty::String);
            ts_fb.push_stmt(MStmt::Assign {
                dest: cat1,
                value: Rvalue::BinOp {
                    op: MBinOp::ConcatStr,
                    lhs: result,
                    rhs: label,
                },
            });

            // Load the field value and concat it.
            let field_val = ts_fb.new_local(field.ty.clone());
            ts_fb.push_stmt(MStmt::Assign {
                dest: field_val,
                value: Rvalue::GetField {
                    receiver: ts_this,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });
            let cat2 = ts_fb.new_local(Ty::String);
            ts_fb.push_stmt(MStmt::Assign {
                dest: cat2,
                value: Rvalue::BinOp {
                    op: MBinOp::ConcatStr,
                    lhs: cat1,
                    rhs: field_val,
                },
            });
            ts_fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Local(cat2),
            });
        }

        // Append closing ")"
        let close_sid = module.intern_string(")");
        let close = ts_fb.new_local(Ty::String);
        ts_fb.push_stmt(MStmt::Assign {
            dest: close,
            value: Rvalue::Const(MirConst::String(close_sid)),
        });
        let final_str = ts_fb.new_local(Ty::String);
        ts_fb.push_stmt(MStmt::Assign {
            dest: final_str,
            value: Rvalue::BinOp {
                op: MBinOp::ConcatStr,
                lhs: result,
                rhs: close,
            },
        });
        ts_fb.set_terminator(Terminator::ReturnValue(final_str));
        mir_methods.push(ts_fb.finish());
    }

    // Replace the stub class with the fully-lowered version.
    module.classes[class_idx] = MirClass {
        name: class_name,
        super_class,
        is_open: c.is_open,
        is_abstract: c.is_abstract,
        is_interface: false,
        interfaces: iface_names,
        fields,
        methods: mir_methods,
        constructor: init_fn,
    };
}

fn lower_interface(
    iface: &skotch_syntax::InterfaceDecl,
    module: &mut MirModule,
    interner: &mut Interner,
) {
    let name = interner.resolve(iface.name).to_string();

    // Interface methods: create MirFunction stubs with is_abstract = true.
    let methods: Vec<MirFunction> = iface
        .methods
        .iter()
        .map(|m| {
            let mname = interner.resolve(m.name).to_string();
            let return_ty = m
                .return_ty
                .as_ref()
                .and_then(|tr| skotch_types::ty_from_name(interner.resolve(tr.name)))
                .unwrap_or(Ty::Unit);
            MirFunction {
                id: FuncId(0),
                name: mname,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: m.is_abstract,
            }
        })
        .collect();

    // Interfaces have no constructor — use a dummy.
    let dummy_init = MirFunction {
        id: FuncId(0),
        name: "<init>".to_string(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: Vec::new(),
        return_ty: Ty::Unit,
        required_params: 0,
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract: false,
    };

    module.classes.push(MirClass {
        name,
        super_class: None,
        is_open: true,
        is_abstract: true,
        is_interface: true,
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods,
        constructor: dummy_init,
    });
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
