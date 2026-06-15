//! Pretty-print MIR functions for debugging.
//!
//! The full module is too noisy for a passing build; the dumper is
//! gated on the `SKOTCH_DUMP_MIR` env var (one or more
//! `Wrapper.method` patterns, comma-separated). Matching functions
//! are pretty-printed to stderr. The intended call sites are the end
//! of `mir-lower::lower_file` and the end of `skotch-compose::compose_transform` —
//! one shot each so you can diff "what mir-lower produced" against
//! "what compose-transform left behind."

use crate::{BinOp, CallKind, MirConst, MirFunction, MirModule, Rvalue, Stmt, Terminator};

/// Read `SKOTCH_DUMP_MIR` once. Returns the comma-separated patterns
/// or `None` if the env var is unset or empty.
pub fn dump_patterns() -> Option<Vec<String>> {
    let raw = std::env::var("SKOTCH_DUMP_MIR").ok()?;
    let parts: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

/// True if `<wrapper>.<func>` matches any user pattern. Supports
/// `Wrapper.method` exact, `*.method` (any wrapper), `Wrapper.*`
/// (every method in wrapper), or `Wrapper` (every method).
pub fn matches_pattern(wrapper: &str, func_name: &str, patterns: &[String]) -> bool {
    for p in patterns {
        let (pw, pm) = match p.split_once('.') {
            Some((w, m)) => (w, m),
            None => (p.as_str(), "*"),
        };
        let wrapper_simple = wrapper.rsplit('/').next().unwrap_or(wrapper);
        let w_ok = pw == "*" || pw == wrapper || pw == wrapper_simple;
        let m_ok = pm == "*" || pm == func_name;
        if w_ok && m_ok {
            return true;
        }
    }
    false
}

/// Pretty-print one function as ~80-col text. The output has no
/// trailing newline so callers can prefix labels.
pub fn format_function(f: &MirFunction, module: &MirModule, phase: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "==== MIR [{phase}] {wrapper}.{name} {ret} ====\n",
        wrapper = module.wrapper_class,
        name = f.name,
        ret = format_ty(&f.return_ty),
    ));
    out.push_str("  params: [");
    let mut first = true;
    for (i, p) in f.params.iter().enumerate() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let name = f.param_names.get(i).map(|s| s.as_str()).unwrap_or("?");
        let ty = f
            .locals
            .get(p.0 as usize)
            .map(format_ty)
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!("{name}:{ty}#L{}", p.0));
    }
    out.push_str("]\n");
    if !f.locals.is_empty() {
        out.push_str("  locals:\n");
        for (i, t) in f.locals.iter().enumerate() {
            out.push_str(&format!("    L{i}: {}\n", format_ty(t)));
        }
    }
    for (bi, block) in f.blocks.iter().enumerate() {
        out.push_str(&format!("  block {bi}:\n"));
        for s in &block.stmts {
            out.push_str("    ");
            out.push_str(&format_stmt(s));
            out.push('\n');
        }
        out.push_str("    ");
        out.push_str(&format_terminator(&block.terminator));
        out.push('\n');
    }
    out
}

fn format_stmt(s: &Stmt) -> String {
    let Stmt::Assign { dest, value } = s;
    format!("L{} = {}", dest.0, format_rvalue(value))
}

fn format_rvalue(r: &Rvalue) -> String {
    match r {
        Rvalue::Const(c) => format!("Const({})", format_const(c)),
        Rvalue::Local(l) => format!("L{}", l.0),
        Rvalue::BinOp { op, lhs, rhs } => {
            format!("BinOp({}, L{}, L{})", format_binop(op), lhs.0, rhs.0)
        }
        Rvalue::GetStaticField {
            class_name,
            field_name,
            descriptor,
        } => {
            format!("getstatic {class_name}.{field_name}:{descriptor}")
        }
        Rvalue::NewInstance(c) => format!("new {c}"),
        Rvalue::GetField {
            receiver,
            class_name,
            field_name,
        } => {
            format!("getfield L{}.{class_name}.{field_name}", receiver.0)
        }
        Rvalue::PutField {
            receiver,
            class_name,
            field_name,
            value,
        } => {
            format!(
                "putfield L{}.{class_name}.{field_name} = L{}",
                receiver.0, value.0
            )
        }
        Rvalue::PutStaticField {
            class_name,
            field_name,
            descriptor,
            value,
        } => {
            format!(
                "putstatic {class_name}.{field_name}:{descriptor} = L{}",
                value.0
            )
        }
        Rvalue::Call { kind, args } => {
            let mut s = format_callkind(kind);
            s.push('(');
            let mut first = true;
            for a in args {
                if !first {
                    s.push_str(", ");
                }
                first = false;
                s.push_str(&format!("L{}", a.0));
            }
            s.push(')');
            s
        }
        Rvalue::InstanceOf {
            obj,
            type_descriptor,
        } => {
            format!("L{} instanceof {type_descriptor}", obj.0)
        }
        Rvalue::NewIntArray(s) => format!("new int[L{}]", s.0),
        Rvalue::ArrayLoad { array, index } => format!("L{}[L{}]", array.0, index.0),
        Rvalue::ArrayStore {
            array,
            index,
            value,
        } => {
            format!("L{}[L{}] = L{}", array.0, index.0, value.0)
        }
        Rvalue::ArrayLength(a) => format!("L{}.length", a.0),
        Rvalue::NewObjectArray(s) => format!("new Object[L{}]", s.0),
        Rvalue::NewTypedObjectArray {
            size,
            element_class,
        } => {
            format!("new {element_class}[L{}]", size.0)
        }
        Rvalue::ObjectArrayStore {
            array,
            index,
            value,
        } => {
            format!("L{}[L{}] = L{}", array.0, index.0, value.0)
        }
        Rvalue::CheckCast { obj, target_class } => {
            format!("(L{} as {target_class})", obj.0)
        }
    }
}

fn format_callkind(k: &CallKind) -> String {
    match k {
        CallKind::Static(id) => format!("static#{}", id.0),
        CallKind::Println => "println".to_string(),
        CallKind::Print => "print".to_string(),
        CallKind::PrintlnConcat => "println-concat".to_string(),
        CallKind::PrintConcat => "print-concat".to_string(),
        CallKind::StaticJava {
            class_name,
            method_name,
            descriptor,
        } => {
            format!("invokestatic {class_name}.{method_name}:{descriptor}")
        }
        CallKind::VirtualJava {
            class_name,
            method_name,
            descriptor,
        } => {
            format!("invokevirtual {class_name}.{method_name}:{descriptor}")
        }
        CallKind::Constructor(c) => format!("new+<init> {c}"),
        CallKind::ConstructorJava {
            class_name,
            descriptor,
        } => {
            format!("new+<init> {class_name}:{descriptor}")
        }
        CallKind::Virtual {
            class_name,
            method_name,
        } => {
            format!("invokevirtual {class_name}.{method_name}")
        }
        CallKind::Super {
            class_name,
            method_name,
        } => {
            format!("invokespecial {class_name}.{method_name}")
        }
        CallKind::MakeConcatWithConstants { recipe, descriptor } => {
            format!("makeConcatWithConstants {recipe:?}:{descriptor}")
        }
        CallKind::LambdaMetafactory {
            arity,
            method_name,
            specialized_descriptor,
            ..
        } => {
            format!(
                "lambda-metafactory arity={arity} impl={method_name} desc={specialized_descriptor}"
            )
        }
        CallKind::FunctionInvoke { arity } => format!("Function{arity}.invoke"),
    }
}

fn format_binop(op: &BinOp) -> &'static str {
    match op {
        BinOp::AddI => "+I",
        BinOp::SubI => "-I",
        BinOp::MulI => "*I",
        BinOp::DivI => "/I",
        BinOp::ModI => "%I",
        BinOp::AddL => "+J",
        BinOp::SubL => "-J",
        BinOp::MulL => "*J",
        BinOp::DivL => "/J",
        BinOp::ModL => "%J",
        BinOp::AddD => "+D",
        BinOp::SubD => "-D",
        BinOp::MulD => "*D",
        BinOp::DivD => "/D",
        BinOp::ModD => "%D",
        BinOp::AddF => "+F",
        BinOp::SubF => "-F",
        BinOp::MulF => "*F",
        BinOp::DivF => "/F",
        BinOp::ModF => "%F",
        BinOp::ConcatStr => "++",
        BinOp::CmpEq => "==",
        BinOp::CmpNe => "!=",
        BinOp::CmpLt => "<",
        BinOp::CmpGt => ">",
        BinOp::CmpLe => "<=",
        BinOp::CmpGe => ">=",
    }
}

fn format_terminator(t: &Terminator) -> String {
    match t {
        Terminator::Return => "return".to_string(),
        Terminator::ReturnValue(l) => format!("return L{}", l.0),
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => {
            format!(
                "if L{} -> block {then_block} else block {else_block}",
                cond.0
            )
        }
        Terminator::Goto(b) => format!("goto block {b}"),
        Terminator::Throw(l) => format!("throw L{}", l.0),
    }
}

fn format_const(c: &MirConst) -> String {
    match c {
        MirConst::Unit => "Unit".to_string(),
        MirConst::Bool(b) => b.to_string(),
        MirConst::Int(i) => i.to_string(),
        MirConst::Long(l) => format!("{l}L"),
        MirConst::Float(f) => format!("{f}f"),
        MirConst::Double(d) => format!("{d}"),
        MirConst::Null => "null".to_string(),
        MirConst::String(s) => format!("str#{}", s.0),
    }
}

fn format_ty(t: &skotch_types::Ty) -> String {
    format!("{t:?}")
}

/// Convenience: if `SKOTCH_DUMP_MIR` matches any of `module`'s
/// functions (by `wrapper_class.func_name`), pretty-print them to
/// stderr with `phase` as a label. Cheap fast-path when the env var
/// is unset.
pub fn maybe_dump_module(module: &MirModule, phase: &str) {
    let Some(patterns) = dump_patterns() else {
        return;
    };
    let mut printed = 0usize;
    let wrapper = module.wrapper_class.as_str();
    for f in &module.functions {
        if matches_pattern(wrapper, &f.name, &patterns) {
            eprintln!("{}", format_function(f, module, phase));
            printed += 1;
        }
    }
    // Also walk classes — lambda invoke bodies live there.
    for c in &module.classes {
        for m in &c.methods {
            let qualified = format!("{}.{}", c.name, m.name);
            // Match either against the class's own simple name or via the wildcard.
            for p in &patterns {
                let (pw, pm) = match p.split_once('.') {
                    Some((w, m)) => (w, m),
                    None => (p.as_str(), "*"),
                };
                let class_simple = c.name.rsplit('/').next().unwrap_or(&c.name);
                // Permissive: pw matches if it equals class_simple OR the
                // class is a synthetic subclass like `Wrapper$Lambda$25`
                // (anchor at start, require `$` after to avoid spurious
                // substring hits).
                let w_ok = pw == "*"
                    || pw == c.name
                    || pw == class_simple
                    || class_simple.starts_with(pw) && class_simple[pw.len()..].starts_with('$');
                let m_ok = pm == "*" || pm == m.name;
                if w_ok && m_ok {
                    let mut mf = m.clone();
                    mf.name = qualified.clone();
                    eprintln!("{}", format_function(&mf, module, phase));
                    printed += 1;
                    break;
                }
            }
        }
    }
    let _ = printed;
}
