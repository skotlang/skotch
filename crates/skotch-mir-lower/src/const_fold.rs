//! Compile-time constant evaluation used during MIR lowering.
//!
//! These helpers fold purely-literal expressions (`1 + 2`, `"a" + "b"`,
//! string templates whose interpolated parts are all constants) so the
//! lowerer can substitute a single `Const` instead of generating arithmetic /
//! concatenation bytecode. The goal is parity with kotlinc, which folds
//! these same shapes at type-check time.

use skotch_intern::Interner;
use skotch_syntax::{BinOp, Expr};

pub(crate) fn try_eval_int(e: &Expr) -> Option<i32> {
    match e {
        Expr::IntLit(v, _) => Some(*v as i32),
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = try_eval_int(lhs)?;
            let r = try_eval_int(rhs)?;
            match op {
                BinOp::Add => Some(l.wrapping_add(r)),
                BinOp::Sub => Some(l.wrapping_sub(r)),
                BinOp::Mul => Some(l.wrapping_mul(r)),
                BinOp::Div if r != 0 => Some(l.wrapping_div(r)),
                BinOp::Mod if r != 0 => Some(l.wrapping_rem(r)),
                _ => None,
            }
        }
        Expr::Unary {
            op: skotch_syntax::UnaryOp::Neg,
            operand,
            ..
        } => {
            let v = try_eval_int(operand)?;
            Some(v.wrapping_neg())
        }
        Expr::Paren(inner, _) => try_eval_int(inner),
        _ => None,
    }
}

/// Try to constant-fold a binary expression with Int operands.
pub(crate) fn try_const_fold_int(lhs: &Expr, rhs: &Expr, op: &BinOp) -> Option<i32> {
    if !matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    ) {
        return None;
    }
    let l = try_eval_int(lhs)?;
    let r = try_eval_int(rhs)?;
    match op {
        BinOp::Add => Some(l.wrapping_add(r)),
        BinOp::Sub => Some(l.wrapping_sub(r)),
        BinOp::Mul => Some(l.wrapping_mul(r)),
        BinOp::Div if r != 0 => Some(l.wrapping_div(r)),
        BinOp::Mod if r != 0 => Some(l.wrapping_rem(r)),
        _ => None,
    }
}

/// Try to evaluate a long constant expression at compile time.
pub(crate) fn try_eval_long(e: &Expr) -> Option<i64> {
    match e {
        Expr::LongLit(v, _) => Some(*v),
        Expr::Binary { op, lhs, rhs, .. } => {
            let l = try_eval_long(lhs)?;
            let r = try_eval_long(rhs)?;
            match op {
                BinOp::Add => Some(l.wrapping_add(r)),
                BinOp::Sub => Some(l.wrapping_sub(r)),
                BinOp::Mul => Some(l.wrapping_mul(r)),
                BinOp::Div if r != 0 => Some(l.wrapping_div(r)),
                BinOp::Mod if r != 0 => Some(l.wrapping_rem(r)),
                _ => None,
            }
        }
        Expr::Paren(inner, _) => try_eval_long(inner),
        _ => None,
    }
}

/// Try to constant-fold a binary expression with Long operands.
pub(crate) fn try_const_fold_long(lhs: &Expr, rhs: &Expr, op: &BinOp) -> Option<i64> {
    if !matches!(
        op,
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
    ) {
        return None;
    }
    // Only fold if at least one operand is a Long literal (otherwise it's an Int expression)
    let has_long = matches!(lhs, Expr::LongLit(..)) || matches!(rhs, Expr::LongLit(..));
    if !has_long {
        return None;
    }
    let l = try_eval_long(lhs)?;
    let r = try_eval_long(rhs)?;
    match op {
        BinOp::Add => Some(l.wrapping_add(r)),
        BinOp::Sub => Some(l.wrapping_sub(r)),
        BinOp::Mul => Some(l.wrapping_mul(r)),
        BinOp::Div if r != 0 => Some(l.wrapping_div(r)),
        BinOp::Mod if r != 0 => Some(l.wrapping_rem(r)),
        _ => None,
    }
}

/// Try to evaluate an expression as a compile-time string constant.
/// Returns the string value if the expression is a constant string,
/// or a string representation of a constant int/long/double/bool.
pub(crate) fn try_eval_string(e: &Expr) -> Option<String> {
    match e {
        Expr::StringLit(s, _) => Some(s.clone()),
        Expr::IntLit(v, _) => Some(v.to_string()),
        Expr::LongLit(v, _) => Some(v.to_string()),
        Expr::DoubleLit(v, _) => Some(format_double_kotlin(*v)),
        Expr::BoolLit(v, _) => Some(v.to_string()),
        Expr::Paren(inner, _) => try_eval_string(inner),
        Expr::Binary { op, lhs, rhs, .. } => {
            // Allow folding of `intExpr + intExpr` etc. inside templates.
            if let Some(v) = try_eval_int(e) {
                return Some(v.to_string());
            }
            if let Some(v) = try_eval_long(e) {
                return Some(v.to_string());
            }
            // String concatenation via +
            if matches!(op, BinOp::Add) {
                let l = try_eval_string(lhs)?;
                let r = try_eval_string(rhs)?;
                return Some(format!("{l}{r}"));
            }
            None
        }
        Expr::Unary { .. } => {
            if let Some(v) = try_eval_int(e) {
                return Some(v.to_string());
            }
            None
        }
        _ => None,
    }
}

/// Format a double the way Kotlin's `toString()` does (e.g., `1.0` not `1`).
pub(crate) fn format_double_kotlin(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if v == v.trunc() && v.abs() < 1e16 {
        // Integer-valued double: kotlin toString prints "1.0"
        return format!("{v:.1}");
    }
    format!("{v}")
}

/// Try to evaluate a string template at compile time. Returns the
/// concatenated string if every part is a compile-time constant.
pub(crate) fn try_const_fold_template(
    parts: &[skotch_syntax::TemplatePart],
    _interner: &Interner,
) -> Option<String> {
    let mut out = String::new();
    for p in parts {
        match p {
            skotch_syntax::TemplatePart::Text(s, _) => out.push_str(s),
            skotch_syntax::TemplatePart::Expr(e) => {
                let s = try_eval_string(e)?;
                out.push_str(&s);
            }
            // Ident references aren't compile-time constants in general.
            skotch_syntax::TemplatePart::IdentRef(..) => return None,
        }
    }
    Some(out)
}
