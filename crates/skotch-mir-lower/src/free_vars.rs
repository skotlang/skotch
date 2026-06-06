//! Free-variable + captured-`this` analysis for lambda lowering.
//!
//! The lambda lowerer needs to know which identifiers in a lambda body
//! reference values from the enclosing scope — those get hoisted into
//! the synthetic invoke method's class as captured fields. Separate
//! from that, it needs to know whether the lambda calls a method on
//! the enclosing class implicitly (`foo()` resolving to `this.foo()`
//! where `this` is the outer class), because that requires capturing
//! the outer `this` reference too.
//!
//! Helpers in this module are pure tree walks over `skotch_syntax::Block`
//! / `Expr`; they don't mutate FnBuilder state and take the locals
//! slice (`&[Ty]`) as data, not as a borrowed FnBuilder, so the module
//! stays decoupled from the rest of the lowerer.

use skotch_intern::{Interner, Symbol};
use skotch_mir::{LocalId, MirModule};
use skotch_syntax::Stmt;
use skotch_types::Ty;

pub(crate) fn collect_free_vars(
    body: &skotch_syntax::Block,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    locals: &[Ty],
    _interner: &Interner,
) -> Vec<(Symbol, LocalId, Ty)> {
    let mut free = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    collect_free_in_block(body, param_names, outer_scope, locals, &mut free, &mut seen);
    free
}

/// Check whether a block references a given identifier (Symbol) anywhere
/// in its expression positions. Used to detect when a lambda body uses
/// the implicit `it` parameter, so the lowering can add the invoke
/// method param even when the surrounding caller-arity heuristic
/// doesn't predict it.
pub(crate) fn body_references_ident(body: &skotch_syntax::Block, target: Symbol) -> bool {
    let mut found = false;
    scan_block_for_ident(body, target, &mut found);
    found
}

fn scan_block_for_ident(block: &skotch_syntax::Block, target: Symbol, found: &mut bool) {
    for stmt in &block.stmts {
        if *found {
            return;
        }
        match stmt {
            skotch_syntax::Stmt::Expr(e) | skotch_syntax::Stmt::Return { value: Some(e), .. } => {
                scan_expr_for_ident(e, target, found);
            }
            skotch_syntax::Stmt::Val(v) => {
                scan_expr_for_ident(&v.init, target, found);
            }
            skotch_syntax::Stmt::Assign { value, .. } => {
                scan_expr_for_ident(value, target, found);
            }
            _ => {}
        }
    }
}

fn scan_expr_for_ident(e: &skotch_syntax::Expr, target: Symbol, found: &mut bool) {
    if *found {
        return;
    }
    use skotch_syntax::Expr;
    match e {
        Expr::Ident(name, _) if *name == target => {
            *found = true;
        }
        Expr::Call { callee, args, .. } => {
            scan_expr_for_ident(callee, target, found);
            for a in args {
                scan_expr_for_ident(&a.expr, target, found);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr_for_ident(lhs, target, found);
            scan_expr_for_ident(rhs, target, found);
        }
        Expr::Unary { operand, .. } => scan_expr_for_ident(operand, target, found),
        Expr::Field { receiver, .. } | Expr::SafeCall { receiver, .. } => {
            scan_expr_for_ident(receiver, target, found);
        }
        Expr::Index {
            receiver, index, ..
        } => {
            scan_expr_for_ident(receiver, target, found);
            scan_expr_for_ident(index, target, found);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            scan_expr_for_ident(cond, target, found);
            scan_block_for_ident(then_block, target, found);
            if let Some(eb) = else_block {
                scan_block_for_ident(eb, target, found);
            }
        }
        Expr::When {
            subject,
            branches,
            else_body,
            ..
        } => {
            scan_expr_for_ident(subject, target, found);
            for br in branches {
                scan_expr_for_ident(&br.pattern, target, found);
                if let Some(re) = &br.range_end {
                    scan_expr_for_ident(re, target, found);
                }
                scan_expr_for_ident(&br.body, target, found);
            }
            if let Some(eb) = else_body {
                scan_expr_for_ident(eb, target, found);
            }
        }
        _ => {}
    }
}

/// Detect whether a lambda body has unqualified method calls (or property
/// accesses) that resolve to members of the enclosing class. Used to
/// decide if the lambda needs to capture outer `this` so those calls
/// can dispatch via the captured field instead of using the lambda's
/// own `this`.
pub(crate) fn body_needs_outer_this(
    body: &skotch_syntax::Block,
    outer_class_name: &str,
    module: &MirModule,
    interner: &mut Interner,
) -> bool {
    let mut method_syms: rustc_hash::FxHashSet<Symbol> = rustc_hash::FxHashSet::default();
    // Walk the class plus its superclass chain and implemented interfaces
    // so that calls to inherited extension methods (e.g. `dp.roundToPx()`
    // on `Density`-extension inherited by a `MyDensity : Density` outer
    // class) are detected.
    let mut to_visit: Vec<String> = vec![outer_class_name.to_string()];
    let mut visited: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
    while let Some(cn) = to_visit.pop() {
        if !visited.insert(cn.clone()) {
            continue;
        }
        if let Some(cls) = module.find_class(&cn) {
            for m in &cls.methods {
                method_syms.insert(interner.intern(&m.name));
            }
            for iname in &cls.interfaces {
                to_visit.push(iname.clone());
            }
            if let Some(sc) = &cls.super_class {
                to_visit.push(sc.clone());
            }
        }
    }
    if method_syms.is_empty() {
        return false;
    }
    let mut found = false;
    scan_block_for_outer_calls(body, &method_syms, &mut found);
    found
}

fn scan_block_for_outer_calls(
    block: &skotch_syntax::Block,
    method_syms: &rustc_hash::FxHashSet<Symbol>,
    found: &mut bool,
) {
    for stmt in &block.stmts {
        if *found {
            return;
        }
        match stmt {
            skotch_syntax::Stmt::Expr(e) | skotch_syntax::Stmt::Return { value: Some(e), .. } => {
                scan_expr_for_outer_calls(e, method_syms, found);
            }
            skotch_syntax::Stmt::Val(v) => {
                scan_expr_for_outer_calls(&v.init, method_syms, found);
            }
            skotch_syntax::Stmt::Assign { value, .. } => {
                scan_expr_for_outer_calls(value, method_syms, found);
            }
            _ => {}
        }
    }
}

fn scan_expr_for_outer_calls(
    e: &skotch_syntax::Expr,
    method_syms: &rustc_hash::FxHashSet<Symbol>,
    found: &mut bool,
) {
    if *found {
        return;
    }
    use skotch_syntax::Expr;
    match e {
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name, _) = callee.as_ref() {
                if method_syms.contains(name) {
                    *found = true;
                    return;
                }
            }
            // Receiver dot-call form: `dp.roundToPx()`. If `roundToPx` is
            // an extension method on the outer `this`'s class (which is
            // what method_syms reflects), kotlinc lowers this as
            // `outer.roundToPx(dp)`, requiring the lambda to capture
            // the outer `this`.
            if let Expr::Field { name, .. } = callee.as_ref() {
                if method_syms.contains(name) {
                    *found = true;
                    return;
                }
            }
            scan_expr_for_outer_calls(callee, method_syms, found);
            for a in args {
                scan_expr_for_outer_calls(&a.expr, method_syms, found);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr_for_outer_calls(lhs, method_syms, found);
            scan_expr_for_outer_calls(rhs, method_syms, found);
        }
        Expr::Unary { operand, .. } => scan_expr_for_outer_calls(operand, method_syms, found),
        Expr::Field { receiver, .. } | Expr::SafeCall { receiver, .. } => {
            scan_expr_for_outer_calls(receiver, method_syms, found);
        }
        Expr::Index {
            receiver, index, ..
        } => {
            scan_expr_for_outer_calls(receiver, method_syms, found);
            scan_expr_for_outer_calls(index, method_syms, found);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            scan_expr_for_outer_calls(cond, method_syms, found);
            scan_block_for_outer_calls(then_block, method_syms, found);
            if let Some(eb) = else_block {
                scan_block_for_outer_calls(eb, method_syms, found);
            }
        }
        _ => {}
    }
}

fn collect_free_in_block(
    block: &skotch_syntax::Block,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    locals: &[Ty],
    free: &mut Vec<(Symbol, LocalId, Ty)>,
    seen: &mut rustc_hash::FxHashSet<Symbol>,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } => {
                collect_free_in_expr(e, param_names, outer_scope, locals, free, seen);
            }
            Stmt::Val(v) => {
                collect_free_in_expr(&v.init, param_names, outer_scope, locals, free, seen);
            }
            Stmt::Assign { value, .. } => {
                collect_free_in_expr(value, param_names, outer_scope, locals, free, seen);
            }
            Stmt::IndexAssign {
                receiver,
                index,
                value,
                ..
            } => {
                collect_free_in_expr(receiver, param_names, outer_scope, locals, free, seen);
                collect_free_in_expr(index, param_names, outer_scope, locals, free, seen);
                collect_free_in_expr(value, param_names, outer_scope, locals, free, seen);
            }
            _ => {}
        }
    }
}

fn collect_free_in_expr(
    e: &skotch_syntax::Expr,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    locals: &[Ty],
    free: &mut Vec<(Symbol, LocalId, Ty)>,
    seen: &mut rustc_hash::FxHashSet<Symbol>,
) {
    use skotch_syntax::Expr;
    match e {
        Expr::Ident(name, _) if !param_names.contains(name) && !seen.contains(name) => {
            if let Some((_, local_id)) = outer_scope.iter().rev().find(|(s, _)| s == name) {
                let ty = locals[local_id.0 as usize].clone();
                free.push((*name, *local_id, ty));
                seen.insert(*name);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_free_in_expr(lhs, param_names, outer_scope, locals, free, seen);
            collect_free_in_expr(rhs, param_names, outer_scope, locals, free, seen);
        }
        Expr::Unary { operand, .. } => {
            collect_free_in_expr(operand, param_names, outer_scope, locals, free, seen);
        }
        Expr::Call { callee, args, .. } => {
            collect_free_in_expr(callee, param_names, outer_scope, locals, free, seen);
            for a in args {
                collect_free_in_expr(&a.expr, param_names, outer_scope, locals, free, seen);
            }
        }
        Expr::Field { receiver, .. } | Expr::SafeCall { receiver, .. } => {
            collect_free_in_expr(receiver, param_names, outer_scope, locals, free, seen);
        }
        Expr::Index {
            receiver, index, ..
        } => {
            collect_free_in_expr(receiver, param_names, outer_scope, locals, free, seen);
            collect_free_in_expr(index, param_names, outer_scope, locals, free, seen);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_free_in_expr(cond, param_names, outer_scope, locals, free, seen);
            collect_free_in_block(then_block, param_names, outer_scope, locals, free, seen);
            if let Some(eb) = else_block {
                collect_free_in_block(eb, param_names, outer_scope, locals, free, seen);
            }
        }
        Expr::StringTemplate(parts, _) => {
            for p in parts {
                match p {
                    skotch_syntax::TemplatePart::Expr(inner) => {
                        collect_free_in_expr(inner, param_names, outer_scope, locals, free, seen);
                    }
                    skotch_syntax::TemplatePart::IdentRef(sym, _)
                        if !param_names.contains(sym) && !seen.contains(sym) =>
                    {
                        if let Some((_, local_id)) =
                            outer_scope.iter().rev().find(|(s, _)| s == sym)
                        {
                            let ty = locals[local_id.0 as usize].clone();
                            free.push((*sym, *local_id, ty));
                            seen.insert(*sym);
                        }
                    }
                    _ => {}
                }
            }
        }
        Expr::Paren(inner, _)
        | Expr::NotNullAssert { expr: inner, .. }
        | Expr::IsCheck { expr: inner, .. }
        | Expr::AsCast { expr: inner, .. }
        | Expr::Throw { expr: inner, .. } => {
            collect_free_in_expr(inner, param_names, outer_scope, locals, free, seen);
        }
        Expr::ElvisOp { lhs, rhs, .. } => {
            collect_free_in_expr(lhs, param_names, outer_scope, locals, free, seen);
            collect_free_in_expr(rhs, param_names, outer_scope, locals, free, seen);
        }
        Expr::When {
            subject,
            branches,
            else_body,
            ..
        } => {
            collect_free_in_expr(subject, param_names, outer_scope, locals, free, seen);
            for br in branches {
                collect_free_in_expr(&br.pattern, param_names, outer_scope, locals, free, seen);
                if let Some(re) = &br.range_end {
                    collect_free_in_expr(re, param_names, outer_scope, locals, free, seen);
                }
                collect_free_in_expr(&br.body, param_names, outer_scope, locals, free, seen);
            }
            if let Some(eb) = else_body {
                collect_free_in_expr(eb, param_names, outer_scope, locals, free, seen);
            }
        }
        Expr::Try {
            body,
            catch_body,
            extra_catches,
            finally_body,
            ..
        } => {
            collect_free_in_block(body, param_names, outer_scope, locals, free, seen);
            if let Some(cb) = catch_body {
                collect_free_in_block(cb, param_names, outer_scope, locals, free, seen);
            }
            for (_, _, eb) in extra_catches {
                collect_free_in_block(eb, param_names, outer_scope, locals, free, seen);
            }
            if let Some(fb_blk) = finally_body {
                collect_free_in_block(fb_blk, param_names, outer_scope, locals, free, seen);
            }
        }
        Expr::Lambda {
            params: inner_params,
            body: inner_body,
            ..
        } => {
            // A nested lambda may reference variables from the enclosing
            // scope.  We must recurse into its body so that those
            // references are surfaced as free variables of the *outer*
            // lambda.  The nested lambda's own parameters shadow outer
            // names, so extend `param_names` with them before recursing.
            let mut extended_params: Vec<Symbol> = param_names.to_vec();
            for p in inner_params {
                extended_params.push(p.name);
            }
            collect_free_in_block(
                inner_body,
                &extended_params,
                outer_scope,
                locals,
                free,
                seen,
            );
        }
        _ => {}
    }
}
