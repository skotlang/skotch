//! Typed-AST entry point for MIR lowering.
//!
//! Parallel to the legacy [`crate::lower_file`] but takes a
//! [`skotch_ast::KtFile`] (typed view over the SIL tree) instead of
//! the Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Initial scaffold returns an empty [`skotch_mir::MirModule`] with
//! the wrapper class name populated. Each consumer migration step
//! expands the coverage one decl/expression form at a time. Same
//! migration pattern as [`skotch_resolve::typed`] and
//! [`skotch_typeck::typed`].

use skotch_ast::{AstNode, KtDecl, KtFile};
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::{BasicBlock, FuncId, MirFunction, MirModule, Terminator};
use skotch_resolve::{PackageSymbolTable, ResolvedFile};
use skotch_typeck::TypedFile;
use skotch_types::Ty;

/// Lower a single typed file to MIR.
///
/// Counterpart of [`crate::lower_file`]. Initial coverage handles
/// the simplest top-level functions; further decl/expression shapes
/// land in subsequent porting sessions.
///
/// ## Current coverage
///
/// - Top-level fun with empty body → MirFunction with a single
///   BasicBlock terminating in `Return`. Parameters / declared
///   types pulled from the typed AST.
///
/// ## Not yet covered
///
/// Every expression form / statement form / class lowering —
/// these are the legacy `lower_*` and `emit_*` functions in
/// `crate::lib.rs` (27k LOC), to be ported one at a time.
pub fn lower_file(
    file: KtFile<'_>,
    _resolved: &ResolvedFile,
    typed: &TypedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    wrapper_class: &str,
    _package_symbols: Option<&PackageSymbolTable>,
) -> MirModule {
    let mut module = MirModule {
        wrapper_class: wrapper_class.to_string(),
        ..MirModule::default()
    };

    // Top-level classes — emit minimal MirClass entries. Body
    // method shapes (empty Return bodies) populated below; method
    // body lowering is deferred to follow-up sessions.
    for decl in file.decls() {
        if let KtDecl::Class(c) = decl {
            let name = match c.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let (super_class, interfaces) = collect_class_super_iface(c.super_type_list());
            let fields = collect_class_fields(c);
            let methods = collect_class_methods(c, &name, &mut module.strings);
            let constructor = constructor_from_primary(c, &name);
            // Companion object handling: if the class body has a
            // `companion object [Name] { ... }`, emit a sibling
            // MirClass `<Outer>$<Companion>` and point the outer's
            // companion_class_name at it.
            let companion = c.body().and_then(|body| {
                body.declarations().find_map(|d| match d {
                    KtDecl::Object(o) if o.is_companion() => Some(o),
                    _ => None,
                })
            });
            let companion_class_name = if let Some(o) = companion {
                let comp_simple = o.name().unwrap_or("Companion").to_string();
                let comp_qname = format!("{}${}", name, comp_simple);
                let comp_methods = collect_object_methods(o, &mut module.strings);
                let comp_class = skotch_mir::MirClass {
                    name: comp_qname.clone(),
                    super_class: None,
                    is_open: false,
                    is_abstract: false,
                    is_interface: false,
                    interfaces: Vec::new(),
                    fields: Vec::new(),
                    methods: comp_methods,
                    constructor: empty_constructor(&comp_qname),
                    secondary_constructors: Vec::new(),
                    is_suspend_lambda: false,
                    is_lambda: false,
                    is_cross_file_stub: false,
                    annotations: Vec::new(),
                    has_type_params: false,
                    is_object_singleton: true,
                    companion_class_name: None,
                    static_fields: Vec::new(),
                    clinit: None,
                };
                Some((comp_qname, comp_class))
            } else {
                None
            };
            let companion_class_name_str = companion_class_name.as_ref().map(|(n, _)| n.clone());
            let mir_class = skotch_mir::MirClass {
                name: name.clone(),
                super_class,
                is_open: c.is_open() || c.is_sealed(),
                is_abstract: c.is_abstract() || c.is_sealed(),
                is_interface: false,
                interfaces,
                fields,
                methods,
                constructor,
                secondary_constructors: collect_secondary_ctors(c),
                is_suspend_lambda: false,
                is_lambda: false,
                is_cross_file_stub: false,
                annotations: Vec::new(),
                has_type_params: c
                    .type_parameter_list()
                    .map(|tpl| tpl.parameters().next().is_some())
                    .unwrap_or(false),
                is_object_singleton: false,
                companion_class_name: companion_class_name_str,
                static_fields: Vec::new(),
                clinit: None,
            };
            module.push_class(mir_class);
            if let Some((_, comp_class)) = companion_class_name {
                module.push_class(comp_class);
            }
            // Nested classes — `class Outer { class Inner { ... } }`
            // becomes a sibling `Outer$Inner` MirClass.
            if let Some(body) = c.body() {
                for d in body.declarations() {
                    if let KtDecl::Class(nested) = d {
                        if let Some(nested_simple) = nested.name() {
                            let nested_qname = format!("{}${}", name, nested_simple);
                            let nested_fields = collect_class_fields(nested);
                            let nested_methods =
                                collect_class_methods(nested, &nested_qname, &mut module.strings);
                            let nested_ctor = constructor_from_primary(nested, &nested_qname);
                            let (n_super, n_ifaces) =
                                collect_class_super_iface(nested.super_type_list());
                            let nested_mir = skotch_mir::MirClass {
                                name: nested_qname.clone(),
                                super_class: n_super,
                                is_open: nested.is_open() || nested.is_sealed(),
                                is_abstract: nested.is_abstract() || nested.is_sealed(),
                                is_interface: false,
                                interfaces: n_ifaces,
                                fields: nested_fields,
                                methods: nested_methods,
                                constructor: nested_ctor,
                                secondary_constructors: collect_secondary_ctors(nested),
                                is_suspend_lambda: false,
                                is_lambda: false,
                                is_cross_file_stub: false,
                                annotations: Vec::new(),
                                has_type_params: nested
                                    .type_parameter_list()
                                    .map(|tpl| tpl.parameters().next().is_some())
                                    .unwrap_or(false),
                                is_object_singleton: false,
                                companion_class_name: None,
                                static_fields: Vec::new(),
                                clinit: None,
                            };
                            module.push_class(nested_mir);
                        }
                    }
                }
            }
        }
    }

    // Top-level interfaces — emit as MirClass with is_interface=true.
    for decl in file.decls() {
        if let KtDecl::Interface(i) = decl {
            let name = match i.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let (_, interfaces) = collect_class_super_iface(i.super_type_list());
            let methods = collect_interface_methods(i, &mut module.strings);
            let mir_class = skotch_mir::MirClass {
                name: name.clone(),
                super_class: None,
                is_open: false,
                is_abstract: true,
                is_interface: true,
                interfaces,
                fields: Vec::new(),
                methods,
                constructor: empty_constructor(&name),
                secondary_constructors: Vec::new(),
                is_suspend_lambda: false,
                is_lambda: false,
                is_cross_file_stub: false,
                annotations: Vec::new(),
                has_type_params: i
                    .type_parameter_list()
                    .map(|tpl| tpl.parameters().next().is_some())
                    .unwrap_or(false),
                is_object_singleton: false,
                companion_class_name: None,
                static_fields: Vec::new(),
                clinit: None,
            };
            module.push_class(mir_class);
        }
    }

    // Top-level object declarations — emit with is_object_singleton.
    for decl in file.decls() {
        if let KtDecl::Object(o) = decl {
            let name = match o.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let (super_class, interfaces) = collect_class_super_iface(o.super_type_list());
            let methods = collect_object_methods(o, &mut module.strings);
            let mir_class = skotch_mir::MirClass {
                name: name.clone(),
                super_class,
                is_open: false,
                is_abstract: false,
                is_interface: false,
                interfaces,
                fields: Vec::new(),
                methods,
                constructor: empty_constructor(&name),
                secondary_constructors: Vec::new(),
                is_suspend_lambda: false,
                is_lambda: false,
                is_cross_file_stub: false,
                annotations: Vec::new(),
                has_type_params: false,
                is_object_singleton: true,
                companion_class_name: None,
                static_fields: Vec::new(),
                clinit: None,
            };
            module.push_class(mir_class);
        }
    }

    // Top-level enum classes — emit MirClass with the entry list as
    // static_fields (one per `RED`, `GREEN`, …). The synthesized
    // <clinit> that constructs and stores each entry is deferred to
    // a follow-up — for now the static_fields signal the JVM
    // backend to emit `ACC_STATIC | ACC_FINAL | ACC_ENUM` fields.
    for decl in file.decls() {
        if let KtDecl::EnumClass(e) = decl {
            let name = match e.name() {
                Some(n) => n.to_string(),
                None => continue,
            };
            let static_fields: Vec<skotch_mir::MirField> = e
                .body()
                .map(|body| {
                    body.enum_entries()
                        .filter_map(|entry| entry.name())
                        .map(|entry_name| skotch_mir::MirField {
                            name: entry_name.to_string(),
                            ty: Ty::Class(name.clone()),
                            is_jvm_field: false,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mir_class = skotch_mir::MirClass {
                name: name.clone(),
                super_class: Some("java/lang/Enum".to_string()),
                is_open: false,
                is_abstract: false,
                is_interface: false,
                interfaces: Vec::new(),
                fields: Vec::new(),
                methods: Vec::new(),
                constructor: empty_constructor(&name),
                secondary_constructors: Vec::new(),
                is_suspend_lambda: false,
                is_lambda: false,
                is_cross_file_stub: false,
                annotations: Vec::new(),
                has_type_params: false,
                is_object_singleton: false,
                companion_class_name: None,
                static_fields,
                clinit: None,
            };
            module.push_class(mir_class);
            module.enum_names.insert(name);
        }
    }

    // Top-level vals — emit as top_level_consts (if `const val`) or
    // top_level_props (otherwise). The actual <clinit> synthesis and
    // get<Name>() accessor emission is deferred to a follow-up port.
    for decl in file.decls() {
        if let KtDecl::Property(p) = decl {
            let Some(name) = p.name() else { continue };
            let ty = p
                .type_reference()
                .and_then(|tr| {
                    // Walk the typed TypeRef to a Ty. We don't yet have
                    // shared TypeRef->Ty here; use the typeck output
                    // when available, or fall back to Ty::Any.
                    let _ = tr;
                    None
                })
                .or_else(|| {
                    // Pull from TypedFile.top_vals if pass-1 typeck
                    // collected the val.
                    typed.top_vals.iter().find_map(|tv| {
                        // We don't track the val name in TypedTopVal;
                        // best-effort: assume source-order matches.
                        // (TypedTopVal.name_index is the index, not a
                        // symbol.) Pull by position below instead.
                        let _ = tv;
                        None
                    })
                })
                .unwrap_or(skotch_types::Ty::Any);
            let init_const = p.initializer().and_then(lower_const_init_typed);
            let entry = (
                name.to_string(),
                ty,
                init_const.unwrap_or(skotch_mir::MirConst::Null),
            );
            if p.is_const() {
                module.top_level_consts.push(entry);
            } else {
                module.top_level_prop_names.insert(name.to_string());
                module.top_level_props.push(entry);
            }
        }
    }

    // First pass: collect all top-level fn names → (FuncId, ret Ty).
    // Needed so body lowering can resolve bare `inner()` calls.
    let mut fn_lookup: rustc_hash::FxHashMap<String, (skotch_mir::FuncId, Ty)> =
        rustc_hash::FxHashMap::default();
    {
        let mut idx = 0u32;
        for decl in file.decls() {
            if let KtDecl::Fun(f) = decl {
                if let Some(name) = f.name() {
                    let typed_fn = typed.functions.iter().find(|tf| tf.name_index == idx);
                    let ret = typed_fn.map(|tf| tf.return_ty.clone()).unwrap_or(Ty::Unit);
                    fn_lookup.insert(name.to_string(), (FuncId(idx), ret));
                }
                idx += 1;
            }
        }
    }

    // Top-level val lookup: name → Ty.
    let mut val_lookup: rustc_hash::FxHashMap<String, Ty> =
        rustc_hash::FxHashMap::default();
    for decl in file.decls() {
        if let KtDecl::Property(p) = decl {
            if let Some(name) = p.name() {
                let ty = match p
                    .type_reference()
                    .and_then(|tr| tr.user_type())
                    .and_then(|u| u.name())
                {
                    Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Any),
                    None => Ty::Any,
                };
                val_lookup.insert(name.to_string(), ty);
            }
        }
    }

    // Top-level functions — one MirFunction per KtFun decl.
    let mut fn_id = 0u32;
    for decl in file.decls() {
        if let KtDecl::Fun(f) = decl {
            let name = f.name().unwrap_or("<anon>").to_string();
            // Pull param/return Ty from the TypedFile pass-1 output if
            // the indices line up.
            let typed_fn = typed.functions.iter().find(|tf| tf.name_index == fn_id);
            let return_ty = typed_fn.map(|tf| tf.return_ty.clone()).unwrap_or(Ty::Unit);
            let param_count = f
                .value_parameter_list()
                .map(|pl| pl.parameters().count())
                .unwrap_or(0);
            let params: Vec<skotch_mir::LocalId> = (0..param_count)
                .map(|i| skotch_mir::LocalId(i as u32))
                .collect();
            let param_tys: Vec<Ty> = typed_fn
                .map(|tf| tf.param_tys.clone())
                .unwrap_or_else(|| (0..param_count).map(|_| Ty::Any).collect());
            let param_names: Vec<String> = f
                .value_parameter_list()
                .map(|pl| {
                    pl.parameters()
                        .map(|p| p.name().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            // Body lowering: expression-bodied fns with a literal
            // expression now emit MStmt::Assign + ReturnValue. Block
            // bodies and non-literal expression bodies still emit an
            // empty Return placeholder.
            let wrapper_class = module.wrapper_class.clone();
            let (blocks, extra_locals) = lower_simple_body(
                f,
                &mut module.strings,
                &fn_lookup,
                &val_lookup,
                &wrapper_class,
            );

            let mut locals = param_tys;
            locals.extend(extra_locals);
            module.functions.push(MirFunction {
                id: FuncId(fn_id),
                name,
                params,
                locals,
                blocks,
                return_ty,
                required_params: param_count,
                param_names,
                param_receiver_types: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: false,
                vararg_index: None,
                exception_handlers: Vec::new(),
                is_suspend: f.is_suspend(),
                is_inline: f.is_inline(),
                has_type_params: f
                    .type_parameter_list()
                    .map(|tpl| tpl.parameters().next().is_some())
                    .unwrap_or(false),
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
                named_locals: Vec::new(),
                is_private: f.visibility() == skotch_syntax::Visibility::Private,
                is_static: false,
                default_call_masks: Vec::new(),
                needs_leading_nop: false,
                local_generic_args: rustc_hash::FxHashMap::default(),
            });
            fn_id += 1;
        }
    }

    module
}

/// Recursively unwrap KtExpr::Parenthesized layers.
fn unwrap_parens<'a>(e: skotch_ast::KtExpr<'a>) -> skotch_ast::KtExpr<'a> {
    use skotch_ast::KtExpr;
    let mut cur = e;
    while let KtExpr::Parenthesized(p) = cur {
        let inner = skotch_ast::children(p.syntax())
            .iter()
            .find_map(KtExpr::cast);
        match inner {
            Some(i) => cur = i,
            None => return cur,
        }
    }
    cur
}

/// Resolve the numeric Ty of an expression operand. Used by binary
/// op lowering to pick the right AddI/AddL/AddD/etc variant.
fn operand_numeric_ty(e: &skotch_ast::KtExpr<'_>, f: skotch_ast::KtFun<'_>) -> Ty {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::Integer(_) => {
            // Suffix detection: 1L → Long, otherwise Int.
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if c.kind == skotch_syntax::SyntaxKind::INTEGER_LITERAL {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            });
            match text {
                Some(t) if t.ends_with('L') || t.ends_with('l') => Ty::Long,
                _ => Ty::Int,
            }
        }
        KtExpr::Float(_) => {
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if matches!(
                    c.kind,
                    skotch_syntax::SyntaxKind::FLOAT_LITERAL
                        | skotch_syntax::SyntaxKind::DOUBLE_LITERAL
                ) {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            });
            match text {
                Some(t) if t.ends_with('f') || t.ends_with('F') => Ty::Float,
                _ => Ty::Double,
            }
        }
        KtExpr::Reference(r) => {
            let Some(name) = r.name() else { return Ty::Any };
            f.value_parameter_list()
                .and_then(|pl| {
                    pl.parameters().find_map(|p| {
                        if p.name() != Some(name) {
                            return None;
                        }
                        let user_type = p
                            .type_reference()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())?;
                        skotch_types::ty_from_name(user_type)
                    })
                })
                .unwrap_or(Ty::Any)
        }
        KtExpr::Parenthesized(p) => skotch_ast::children(p.syntax())
            .iter()
            .find_map(KtExpr::cast)
            .map(|inner| operand_numeric_ty(&inner, f))
            .unwrap_or(Ty::Any),
        _ => Ty::Any,
    }
}

/// Promote two operand Tys to the dominant numeric Ty per Kotlin's
/// promotion rules: Double > Float > Long > Int.
fn promote_numeric(a: &Ty, b: &Ty) -> Ty {
    match (a, b) {
        (Ty::Double, _) | (_, Ty::Double) => Ty::Double,
        (Ty::Float, _) | (_, Ty::Float) => Ty::Float,
        (Ty::Long, _) | (_, Ty::Long) => Ty::Long,
        _ => Ty::Int,
    }
}

/// Detect when an expression operand is statically a String — used
/// by binary `+` lowering to choose ConcatStr instead of AddI.
fn operand_is_string(e: &skotch_ast::KtExpr<'_>, f: skotch_ast::KtFun<'_>) -> bool {
    use skotch_ast::KtExpr;
    match e {
        KtExpr::String(_) => true,
        KtExpr::Reference(r) => {
            // Check whether the named parameter has declared type
            // String.
            let Some(name) = r.name() else { return false };
            f.value_parameter_list()
                .map(|pl| {
                    pl.parameters().any(|p| {
                        p.name() == Some(name)
                            && p.type_reference()
                                .and_then(|tr| tr.user_type())
                                .and_then(|u| u.name())
                                == Some("String")
                    })
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Map a `Ty` to a JVM field descriptor.
fn ty_to_descriptor(ty: &Ty) -> String {
    match ty {
        Ty::Int => "I".to_string(),
        Ty::Long => "J".to_string(),
        Ty::Float => "F".to_string(),
        Ty::Double => "D".to_string(),
        Ty::Bool => "Z".to_string(),
        Ty::Char => "C".to_string(),
        Ty::Byte => "B".to_string(),
        Ty::Short => "S".to_string(),
        Ty::Unit => "V".to_string(),
        Ty::String => "Ljava/lang/String;".to_string(),
        Ty::Class(name) => format!("L{name};"),
        _ => "Ljava/lang/Object;".to_string(),
    }
}

/// Map a `MirConst` to its surface Ty.
fn const_ty(c: &skotch_mir::MirConst) -> Ty {
    match c {
        skotch_mir::MirConst::Unit => Ty::Unit,
        skotch_mir::MirConst::Bool(_) => Ty::Bool,
        skotch_mir::MirConst::Int(_) => Ty::Int,
        skotch_mir::MirConst::Long(_) => Ty::Long,
        skotch_mir::MirConst::Float(_) => Ty::Float,
        skotch_mir::MirConst::Double(_) => Ty::Double,
        skotch_mir::MirConst::Null => Ty::Nullable(Box::new(Ty::Any)),
        skotch_mir::MirConst::String(_) => Ty::String,
    }
}

/// Try to lower `fun f() { while (cond) { /* empty */ } }` to a
/// simple 3-block loop CFG:
///   block 0: cond eval + Branch(then=1, exit=2)
///   block 1: Goto(0) — backward jump back to the condition
///   block 2: Return — loop exit
fn try_lower_while_loop(
    block: skotch_ast::KtBlock<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    _fn_lookup: &rustc_hash::FxHashMap<String, (skotch_mir::FuncId, Ty)>,
) -> Option<(Vec<BasicBlock>, Vec<Ty>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Rvalue, Stmt as MStmt};

    let stmts: Vec<KtExpr<'_>> = block.statements().collect();
    if stmts.len() != 1 {
        return None;
    }
    let KtExpr::While(w) = &stmts[0] else {
        return None;
    };
    let cond_expr = w.condition().and_then(|c| c.expression()).map(unwrap_parens)?;
    // Loop body must be a block. We support either empty or a single
    // println / print call with a literal arg.
    let body_block = w.body().and_then(|b| match b.expression()? {
        KtExpr::Block(bl) => Some(bl),
        _ => None,
    })?;
    let body_stmts: Vec<KtExpr<'_>> = body_block.statements().collect();
    if body_stmts.len() > 1 {
        return None;
    }

    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let outer_param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    // Condition must be a binary comparison literal/ref operands.
    let KtExpr::Binary(b) = cond_expr else {
        return None;
    };
    let cmp_op = b.operation().map(|o| o.text()).unwrap_or_default();
    let mir_op = match cmp_op.as_str() {
        "==" => skotch_mir::BinOp::CmpEq,
        "!=" => skotch_mir::BinOp::CmpNe,
        "<" => skotch_mir::BinOp::CmpLt,
        ">" => skotch_mir::BinOp::CmpGt,
        "<=" => skotch_mir::BinOp::CmpLe,
        ">=" => skotch_mir::BinOp::CmpGe,
        _ => return None,
    };

    let mut next_slot = param_count as u32;
    let mut extra_locals: Vec<Ty> = Vec::new();
    let mut cond_stmts: Vec<MStmt> = Vec::new();

    let resolve_op = |e: KtExpr<'_>,
                      next_slot: &mut u32,
                      pre: &mut Vec<MStmt>,
                      locals: &mut Vec<Ty>,
                      strings: &mut Vec<String>|
     -> Option<LocalId> {
        let e = unwrap_parens(e);
        match e {
            KtExpr::Reference(r) => {
                let n = r.name()?;
                let idx = outer_param_names.iter().position(|p| p == n)?;
                Some(LocalId(idx as u32))
            }
            other => {
                let (k, ty) = literal_to_const(&other, strings)?;
                let slot = LocalId(*next_slot);
                *next_slot += 1;
                locals.push(ty);
                pre.push(MStmt::Assign {
                    dest: slot,
                    value: Rvalue::Const(k),
                });
                Some(slot)
            }
        }
    };

    let lhs = resolve_op(
        b.lhs()?,
        &mut next_slot,
        &mut cond_stmts,
        &mut extra_locals,
        strings,
    )?;
    let rhs = resolve_op(
        b.rhs()?,
        &mut next_slot,
        &mut cond_stmts,
        &mut extra_locals,
        strings,
    )?;
    let cmp_slot = LocalId(next_slot);
    extra_locals.push(Ty::Bool);
    cond_stmts.push(MStmt::Assign {
        dest: cmp_slot,
        value: Rvalue::BinOp {
            op: mir_op,
            lhs,
            rhs,
        },
    });

    // Body stmts (println-call or empty).
    let mut body_stmts_mir: Vec<MStmt> = Vec::new();
    if !body_stmts.is_empty() {
        let KtExpr::Call(call) = &body_stmts[0] else {
            return None;
        };
        let name = match call.callee() {
            Some(KtExpr::Reference(r)) => r.name(),
            _ => None,
        }?;
        let kind = match name {
            "println" => skotch_mir::CallKind::Println,
            "print" => skotch_mir::CallKind::Print,
            _ => return None,
        };
        let args = call.value_argument_list()?;
        let arg_exprs: Vec<KtExpr<'_>> =
            args.arguments().filter_map(|a| a.expression()).collect();
        if arg_exprs.len() != 1 {
            return None;
        }
        let (k, ty) = literal_to_const(&arg_exprs[0], strings)?;
        let arg_slot = LocalId(next_slot + 1);
        next_slot += 1;
        extra_locals.push(ty);
        body_stmts_mir.push(MStmt::Assign {
            dest: arg_slot,
            value: Rvalue::Const(k),
        });
        let result_slot = LocalId(next_slot + 1);
        extra_locals.push(Ty::Unit);
        body_stmts_mir.push(MStmt::Assign {
            dest: result_slot,
            value: Rvalue::Call {
                kind,
                args: vec![arg_slot],
            },
        });
    }

    let blocks = vec![
        BasicBlock {
            stmts: cond_stmts,
            terminator: Terminator::Branch {
                cond: cmp_slot,
                then_block: 1,
                else_block: 2,
            },
        },
        BasicBlock {
            stmts: body_stmts_mir,
            terminator: Terminator::Goto(0),
        },
        BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        },
    ];
    Some((blocks, extra_locals))
}

/// Try to lower `do { body } while (cond)` to a 3-block loop CFG
/// where the body block runs first, then the cond block branches
/// back to the body or out to the exit.
fn try_lower_do_while_loop(
    block: skotch_ast::KtBlock<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
) -> Option<(Vec<BasicBlock>, Vec<Ty>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Rvalue, Stmt as MStmt};

    let stmts: Vec<KtExpr<'_>> = block.statements().collect();
    if stmts.len() != 1 {
        return None;
    }
    let KtExpr::DoWhile(dw) = &stmts[0] else {
        return None;
    };
    let cond_expr = dw.condition().and_then(|c| c.expression()).map(unwrap_parens)?;
    let body_block = dw.body().and_then(|b| match b.expression()? {
        KtExpr::Block(bl) => Some(bl),
        _ => None,
    })?;
    let body_stmts: Vec<KtExpr<'_>> = body_block.statements().collect();
    if body_stmts.len() > 1 {
        return None;
    }

    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let outer_param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    let KtExpr::Binary(b) = cond_expr else {
        return None;
    };
    let cmp_op = b.operation().map(|o| o.text()).unwrap_or_default();
    let mir_op = match cmp_op.as_str() {
        "==" => skotch_mir::BinOp::CmpEq,
        "!=" => skotch_mir::BinOp::CmpNe,
        "<" => skotch_mir::BinOp::CmpLt,
        ">" => skotch_mir::BinOp::CmpGt,
        "<=" => skotch_mir::BinOp::CmpLe,
        ">=" => skotch_mir::BinOp::CmpGe,
        _ => return None,
    };

    let mut next_slot = param_count as u32;
    let mut extra_locals: Vec<Ty> = Vec::new();

    // Body stmts.
    let mut body_stmts_mir: Vec<MStmt> = Vec::new();
    if !body_stmts.is_empty() {
        let KtExpr::Call(call) = &body_stmts[0] else {
            return None;
        };
        let name = match call.callee() {
            Some(KtExpr::Reference(r)) => r.name(),
            _ => None,
        }?;
        let kind = match name {
            "println" => skotch_mir::CallKind::Println,
            "print" => skotch_mir::CallKind::Print,
            _ => return None,
        };
        let args = call.value_argument_list()?;
        let arg_exprs: Vec<KtExpr<'_>> =
            args.arguments().filter_map(|a| a.expression()).collect();
        if arg_exprs.len() != 1 {
            return None;
        }
        let (k, ty) = literal_to_const(&arg_exprs[0], strings)?;
        let arg_slot = LocalId(next_slot);
        next_slot += 1;
        extra_locals.push(ty);
        body_stmts_mir.push(MStmt::Assign {
            dest: arg_slot,
            value: Rvalue::Const(k),
        });
        let result_slot = LocalId(next_slot);
        next_slot += 1;
        extra_locals.push(Ty::Unit);
        body_stmts_mir.push(MStmt::Assign {
            dest: result_slot,
            value: Rvalue::Call {
                kind,
                args: vec![arg_slot],
            },
        });
    }

    // Cond stmts (block 1).
    let mut cond_stmts: Vec<MStmt> = Vec::new();
    let resolve_op = |e: KtExpr<'_>,
                      next_slot: &mut u32,
                      pre: &mut Vec<MStmt>,
                      locals: &mut Vec<Ty>,
                      strings: &mut Vec<String>|
     -> Option<LocalId> {
        let e = unwrap_parens(e);
        match e {
            KtExpr::Reference(r) => {
                let n = r.name()?;
                let idx = outer_param_names.iter().position(|p| p == n)?;
                Some(LocalId(idx as u32))
            }
            other => {
                let (k, ty) = literal_to_const(&other, strings)?;
                let slot = LocalId(*next_slot);
                *next_slot += 1;
                locals.push(ty);
                pre.push(MStmt::Assign {
                    dest: slot,
                    value: Rvalue::Const(k),
                });
                Some(slot)
            }
        }
    };
    let lhs = resolve_op(
        b.lhs()?,
        &mut next_slot,
        &mut cond_stmts,
        &mut extra_locals,
        strings,
    )?;
    let rhs = resolve_op(
        b.rhs()?,
        &mut next_slot,
        &mut cond_stmts,
        &mut extra_locals,
        strings,
    )?;
    let cmp_slot = LocalId(next_slot);
    extra_locals.push(Ty::Bool);
    cond_stmts.push(MStmt::Assign {
        dest: cmp_slot,
        value: Rvalue::BinOp {
            op: mir_op,
            lhs,
            rhs,
        },
    });

    let blocks = vec![
        BasicBlock {
            stmts: body_stmts_mir,
            terminator: Terminator::Goto(1),
        },
        BasicBlock {
            stmts: cond_stmts,
            terminator: Terminator::Branch {
                cond: cmp_slot,
                then_block: 0,
                else_block: 2,
            },
        },
        BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        },
    ];
    Some((blocks, extra_locals))
}

/// Try to lower a simple `when (subject) { v1 -> r1; v2 -> r2; else -> default }`
/// expression body. Each arm becomes a 3-block chain:
///   cmp_block: cmp_local = CmpEq(subject, v_i); Branch(then_i, next_cmp)
///   then_block: result = r_i; Goto(join_block)
/// The final block is the ReturnValue join.
fn try_lower_when_expression(
    w: &skotch_ast::KtWhen<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
) -> Option<(Vec<BasicBlock>, Vec<Ty>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Rvalue, Stmt as MStmt};

    let subject = w.subject().map(unwrap_parens)?;
    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let outer_param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    // Subject must be a Reference to a parameter.
    let subject_slot = match &subject {
        KtExpr::Reference(r) => r
            .name()
            .and_then(|n| outer_param_names.iter().position(|p| p == n))
            .map(|i| LocalId(i as u32))?,
        _ => return None,
    };

    // Collect arms (and optional else).
    let mut arms: Vec<(KtExpr<'_>, KtExpr<'_>)> = Vec::new();
    let mut else_arm: Option<KtExpr<'_>> = None;
    for entry in w.entries() {
        if entry.is_else() {
            else_arm = Some(entry.body()?);
            continue;
        }
        let conds = entry.conditions();
        if conds.len() != 1 {
            return None;
        }
        // Condition must be WHEN_CONDITION_WITH_EXPRESSION carrying a literal.
        if conds[0].kind != skotch_syntax::SyntaxKind::WHEN_CONDITION_WITH_EXPRESSION {
            return None;
        }
        let cond_expr = skotch_ast::children(conds[0])
            .iter()
            .find_map(KtExpr::cast)
            .map(unwrap_parens)?;
        let body = entry.body().map(unwrap_parens)?;
        arms.push((cond_expr, body));
    }
    let else_body = else_arm.map(unwrap_parens)?; // require else

    let mut next_slot = param_count as u32;
    let mut extra_locals: Vec<Ty> = Vec::new();
    let result_slot = LocalId(next_slot);
    next_slot += 1;
    // Result type from else_body shape (best-effort).
    let result_ty = match &else_body {
        KtExpr::String(_) => Ty::String,
        KtExpr::Integer(_) => Ty::Int,
        KtExpr::Boolean(_) => Ty::Bool,
        _ => Ty::Any,
    };
    extra_locals.push(result_ty);

    let mut blocks: Vec<BasicBlock> = Vec::new();

    // Each arm contributes: cmp_block (with stmts) + then_block.
    // After all arms, an else_block, then join_block.
    let n_arms = arms.len();
    // Reserve indices: 0..(2N) for cmp/then alternation, 2N for else, 2N+1 for join.
    let else_block_idx = (2 * n_arms) as u32;
    let join_block_idx = else_block_idx + 1;

    for (i, (cond_expr, body)) in arms.iter().enumerate() {
        let mut cmp_stmts: Vec<MStmt> = Vec::new();
        // Lower the literal to a Const slot.
        let (k, ty) = literal_to_const(cond_expr, strings)?;
        let lit_slot = LocalId(next_slot);
        next_slot += 1;
        extra_locals.push(ty);
        cmp_stmts.push(MStmt::Assign {
            dest: lit_slot,
            value: Rvalue::Const(k),
        });
        let cmp_slot = LocalId(next_slot);
        next_slot += 1;
        extra_locals.push(Ty::Bool);
        cmp_stmts.push(MStmt::Assign {
            dest: cmp_slot,
            value: Rvalue::BinOp {
                op: skotch_mir::BinOp::CmpEq,
                lhs: subject_slot,
                rhs: lit_slot,
            },
        });
        let then_block_idx = (2 * i + 1) as u32;
        let next_cmp_block_idx = if i + 1 < n_arms {
            (2 * (i + 1)) as u32
        } else {
            else_block_idx
        };
        blocks.push(BasicBlock {
            stmts: cmp_stmts,
            terminator: Terminator::Branch {
                cond: cmp_slot,
                then_block: then_block_idx,
                else_block: next_cmp_block_idx,
            },
        });

        // then_block: result_slot = literal_from_body; Goto join.
        let (bk, bty) = literal_to_const(body, strings)?;
        let body_slot = LocalId(next_slot);
        next_slot += 1;
        extra_locals.push(bty);
        let then_stmts = vec![
            MStmt::Assign {
                dest: body_slot,
                value: Rvalue::Const(bk),
            },
            MStmt::Assign {
                dest: result_slot,
                value: Rvalue::Local(body_slot),
            },
        ];
        blocks.push(BasicBlock {
            stmts: then_stmts,
            terminator: Terminator::Goto(join_block_idx),
        });
    }

    // else_block.
    let (ek, ety) = literal_to_const(&else_body, strings)?;
    let else_slot = LocalId(next_slot);
    extra_locals.push(ety);
    let else_stmts = vec![
        MStmt::Assign {
            dest: else_slot,
            value: Rvalue::Const(ek),
        },
        MStmt::Assign {
            dest: result_slot,
            value: Rvalue::Local(else_slot),
        },
    ];
    blocks.push(BasicBlock {
        stmts: else_stmts,
        terminator: Terminator::Goto(join_block_idx),
    });

    // join_block.
    blocks.push(BasicBlock {
        stmts: Vec::new(),
        terminator: Terminator::ReturnValue(result_slot),
    });

    Some((blocks, extra_locals))
}

/// Try to lower a simple `if (cond) then-arm else else-arm` expression
/// body. Returns None when the if's condition / arms / else are not
/// simple binary-comparison + literal/ref arms.
fn try_lower_if_expression(
    if_e: &skotch_ast::KtIf<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    _fn_lookup: &rustc_hash::FxHashMap<String, (skotch_mir::FuncId, Ty)>,
) -> Option<(Vec<BasicBlock>, Vec<Ty>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Rvalue, Stmt as MStmt};

    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let outer_param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    // Condition must be a binary comparison between two refs / literals.
    let cond_expr = if_e.condition().and_then(|c| c.expression())?;
    let cond_expr = unwrap_parens(cond_expr);
    let cmp = match cond_expr {
        KtExpr::Binary(b) => b,
        _ => return None,
    };
    let cmp_op = cmp.operation().map(|o| o.text()).unwrap_or_default();
    let cmp_mir_op = match cmp_op.as_str() {
        "==" => skotch_mir::BinOp::CmpEq,
        "!=" => skotch_mir::BinOp::CmpNe,
        "<" => skotch_mir::BinOp::CmpLt,
        ">" => skotch_mir::BinOp::CmpGt,
        "<=" => skotch_mir::BinOp::CmpLe,
        ">=" => skotch_mir::BinOp::CmpGe,
        _ => return None,
    };

    let resolve_operand = |e: skotch_ast::KtExpr<'_>,
                           next_slot: &mut u32,
                           pre_stmts: &mut Vec<MStmt>,
                           extra_locals: &mut Vec<Ty>,
                           strings: &mut Vec<String>|
     -> Option<LocalId> {
        let e = unwrap_parens(e);
        match e {
            KtExpr::Reference(r) => {
                let n = r.name()?;
                let idx = outer_param_names.iter().position(|p| p == n)?;
                Some(LocalId(idx as u32))
            }
            other => {
                let (k, ty) = literal_to_const(&other, strings)?;
                let slot = LocalId(*next_slot);
                *next_slot += 1;
                extra_locals.push(ty);
                pre_stmts.push(MStmt::Assign {
                    dest: slot,
                    value: Rvalue::Const(k),
                });
                Some(slot)
            }
        }
    };

    let then_expr = if_e.then_branch().and_then(|t| t.expression())?;
    let then_expr = unwrap_parens(then_expr);
    let else_expr = if_e.else_branch().and_then(|e| e.expression())?;
    let else_expr = unwrap_parens(else_expr);

    let mut next_slot = param_count as u32;
    let mut extra_locals: Vec<Ty> = Vec::new();

    // Build the condition statements.
    let mut b0_stmts: Vec<MStmt> = Vec::new();
    let lhs = resolve_operand(
        cmp.lhs()?,
        &mut next_slot,
        &mut b0_stmts,
        &mut extra_locals,
        strings,
    )?;
    let rhs = resolve_operand(
        cmp.rhs()?,
        &mut next_slot,
        &mut b0_stmts,
        &mut extra_locals,
        strings,
    )?;
    let cond_slot = LocalId(next_slot);
    next_slot += 1;
    extra_locals.push(Ty::Bool);
    b0_stmts.push(MStmt::Assign {
        dest: cond_slot,
        value: Rvalue::BinOp {
            op: cmp_mir_op,
            lhs,
            rhs,
        },
    });

    // Reserve the result_slot before lowering arms; both arms write
    // into the same slot.
    let result_slot = LocalId(next_slot);
    next_slot += 1;
    // Pick the arm type from the then-branch (best effort).
    let arm_ty = match &then_expr {
        KtExpr::Reference(rr) => rr
            .name()
            .and_then(|n| {
                f.value_parameter_list().and_then(|pl| {
                    pl.parameters().find_map(|p| {
                        if p.name() != Some(n) {
                            return None;
                        }
                        p.type_reference()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                            .and_then(skotch_types::ty_from_name)
                    })
                })
            })
            .unwrap_or(Ty::Any),
        _ => Ty::Any,
    };
    extra_locals.push(arm_ty.clone());

    // Build the then arm.
    let mut b1_stmts: Vec<MStmt> = Vec::new();
    let then_slot = resolve_operand(
        then_expr,
        &mut next_slot,
        &mut b1_stmts,
        &mut extra_locals,
        strings,
    )?;
    b1_stmts.push(MStmt::Assign {
        dest: result_slot,
        value: Rvalue::Local(then_slot),
    });

    // Build the else arm.
    let mut b2_stmts: Vec<MStmt> = Vec::new();
    let else_slot = resolve_operand(
        else_expr,
        &mut next_slot,
        &mut b2_stmts,
        &mut extra_locals,
        strings,
    )?;
    b2_stmts.push(MStmt::Assign {
        dest: result_slot,
        value: Rvalue::Local(else_slot),
    });

    let blocks = vec![
        BasicBlock {
            stmts: b0_stmts,
            terminator: Terminator::Branch {
                cond: cond_slot,
                then_block: 1,
                else_block: 2,
            },
        },
        BasicBlock {
            stmts: b1_stmts,
            terminator: Terminator::Goto(3),
        },
        BasicBlock {
            stmts: b2_stmts,
            terminator: Terminator::Goto(3),
        },
        BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::ReturnValue(result_slot),
        },
    ];
    Some((blocks, extra_locals))
}

/// Try to lower a multi-statement block body using a simple local
/// tracking pass. Handles bodies whose statements are sequences of:
///   - val <name> = <literal>            (KtProperty)
///   - println(<ref-or-literal>)         (single-arg println call)
///   - print(<ref-or-literal>)           (single-arg print call)
///
/// Returns None for any unsupported statement.
fn try_lower_multi_stmt_block_inner(
    block: skotch_ast::KtBlock<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    fn_lookup: &rustc_hash::FxHashMap<String, (skotch_mir::FuncId, Ty)>,
) -> Option<(Vec<BasicBlock>, Vec<Ty>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Stmt as MStmt};

    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    // Param names → their slots.
    let param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut name_to_local: Vec<(String, LocalId)> = param_names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), LocalId(i as u32)))
        .collect();

    let mut local_tys: Vec<Ty> = Vec::new();
    let mut stmts: Vec<MStmt> = Vec::new();
    let mut next_slot: u32 = param_count as u32;

    let mut explicit_return_slot: Option<LocalId> = None;
    for c in skotch_ast::children(block.syntax()) {
        if let Some(prop) = skotch_ast::KtProperty::cast(c) {
            // `val <name> = <literal>` — emit Assign + push local.
            // `val <name> = <a + b>` — emit BinOp Assign.
            let name = prop.name()?;
            let init = prop.initializer()?;
            let init = unwrap_parens(init);
            // Try literal first.
            if let Some((k, ty)) = literal_to_const(&init, strings) {
                let slot = LocalId(next_slot);
                next_slot += 1;
                local_tys.push(ty);
                stmts.push(MStmt::Assign {
                    dest: slot,
                    value: skotch_mir::Rvalue::Const(k),
                });
                name_to_local.push((name.to_string(), slot));
                continue;
            }
            // Try static call: `val x = helper()` where helper is a
            // top-level fn in fn_lookup. Only zero-arg calls.
            if let KtExpr::Call(call) = &init {
                if let Some(KtExpr::Reference(rc)) = call.callee() {
                    if let Some(callee_name) = rc.name() {
                        if let Some((fid, ret)) = fn_lookup.get(callee_name) {
                            let arg_count =
                                call.value_argument_list().map(|a| a.arguments().count()).unwrap_or(0);
                            if arg_count == 0 {
                                let slot = LocalId(next_slot);
                                next_slot += 1;
                                local_tys.push(ret.clone());
                                stmts.push(MStmt::Assign {
                                    dest: slot,
                                    value: skotch_mir::Rvalue::Call {
                                        kind: skotch_mir::CallKind::Static(*fid),
                                        args: Vec::new(),
                                    },
                                });
                                name_to_local.push((name.to_string(), slot));
                                continue;
                            }
                        }
                    }
                }
            }
            // Try binary op on refs/literals.
            if let KtExpr::Binary(b) = &init {
                let op_text = b.operation().map(|o| o.text()).unwrap_or_default();
                let mir_op = match op_text.as_str() {
                    "+" => Some(skotch_mir::BinOp::AddI),
                    "-" => Some(skotch_mir::BinOp::SubI),
                    "*" => Some(skotch_mir::BinOp::MulI),
                    "/" => Some(skotch_mir::BinOp::DivI),
                    "%" => Some(skotch_mir::BinOp::ModI),
                    "==" => Some(skotch_mir::BinOp::CmpEq),
                    "!=" => Some(skotch_mir::BinOp::CmpNe),
                    "<" => Some(skotch_mir::BinOp::CmpLt),
                    ">" => Some(skotch_mir::BinOp::CmpGt),
                    "<=" => Some(skotch_mir::BinOp::CmpLe),
                    ">=" => Some(skotch_mir::BinOp::CmpGe),
                    _ => None,
                };
                if let Some(op) = mir_op {
                    let resolve = |e: KtExpr<'_>,
                                   name_to_local: &[(String, LocalId)]|
                     -> Option<LocalId> {
                        match unwrap_parens(e) {
                            KtExpr::Reference(rr) => {
                                let n = rr.name()?;
                                name_to_local
                                    .iter()
                                    .rev()
                                    .find(|(name, _)| name == n)
                                    .map(|(_, l)| *l)
                            }
                            _ => None,
                        }
                    };
                    let lhs = resolve(b.lhs()?, &name_to_local)?;
                    let rhs = resolve(b.rhs()?, &name_to_local)?;
                    let slot = LocalId(next_slot);
                    next_slot += 1;
                    let is_cmp = matches!(
                        op,
                        skotch_mir::BinOp::CmpEq
                            | skotch_mir::BinOp::CmpNe
                            | skotch_mir::BinOp::CmpLt
                            | skotch_mir::BinOp::CmpGt
                            | skotch_mir::BinOp::CmpLe
                            | skotch_mir::BinOp::CmpGe
                    );
                    local_tys.push(if is_cmp { Ty::Bool } else { Ty::Int });
                    stmts.push(MStmt::Assign {
                        dest: slot,
                        value: skotch_mir::Rvalue::BinOp { op, lhs, rhs },
                    });
                    name_to_local.push((name.to_string(), slot));
                    continue;
                }
            }
            return None;
        }
        if let Some(expr) = KtExpr::cast(c) {
            // Handle trailing `return <ref>`. The ref must be in
            // name_to_local (a known local/param).
            if let KtExpr::Return(r) = &expr {
                let inner = skotch_ast::children(r.syntax())
                    .iter()
                    .find_map(KtExpr::cast)
                    .map(unwrap_parens);
                if let Some(KtExpr::Reference(rr)) = inner {
                    let rn = rr.name()?;
                    let slot = name_to_local
                        .iter()
                        .rev()
                        .find(|(n, _)| n == rn)
                        .map(|(_, l)| *l)?;
                    explicit_return_slot = Some(slot);
                    continue;
                }
                return None;
            }
            // Currently only handle `println(...)` / `print(...)` calls.
            match expr {
                KtExpr::Call(call) => {
                    let name = match call.callee() {
                        Some(KtExpr::Reference(r)) => r.name(),
                        _ => None,
                    }?;
                    let kind = match name {
                        "println" => skotch_mir::CallKind::Println,
                        "print" => skotch_mir::CallKind::Print,
                        _ => return None,
                    };
                    let args = call.value_argument_list()?;
                    let arg_exprs: Vec<KtExpr<'_>> =
                        args.arguments().filter_map(|a| a.expression()).collect();
                    if arg_exprs.len() != 1 {
                        return None;
                    }
                    // Arg is either a Reference to a known local /
                    // param, or a literal that we intern.
                    let arg_slot = match &arg_exprs[0] {
                        KtExpr::Reference(r) => {
                            let n = r.name()?;
                            name_to_local
                                .iter()
                                .rev()
                                .find(|(name, _)| name == n)
                                .map(|(_, l)| *l)?
                        }
                        other => {
                            let (k, ty) = literal_to_const(other, strings)?;
                            let slot = LocalId(next_slot);
                            next_slot += 1;
                            local_tys.push(ty);
                            stmts.push(MStmt::Assign {
                                dest: slot,
                                value: skotch_mir::Rvalue::Const(k),
                            });
                            slot
                        }
                    };
                    let result_slot = LocalId(next_slot);
                    next_slot += 1;
                    local_tys.push(Ty::Unit);
                    stmts.push(MStmt::Assign {
                        dest: result_slot,
                        value: skotch_mir::Rvalue::Call {
                            kind,
                            args: vec![arg_slot],
                        },
                    });
                }
                _ => return None,
            }
        }
    }

    if stmts.is_empty() {
        return None;
    }
    let terminator = match explicit_return_slot {
        Some(slot) => Terminator::ReturnValue(slot),
        None => Terminator::Return,
    };
    let blocks = vec![BasicBlock { stmts, terminator }];
    Some((blocks, local_tys))
}

/// Try to lower `println("Hello, $name")` (a string template with
/// interpolations) to CallKind::PrintlnConcat. Each part (literal
/// chunk or interpolated identifier) becomes a separate arg. Returns
/// None when the template doesn't fit this shape.
fn try_lower_println_template(
    call: &skotch_ast::KtCallExpression<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    next_slot: &mut u32,
    pre_stmts: &mut Vec<skotch_mir::Stmt>,
    extra_locals: &mut Vec<Ty>,
) -> Option<(skotch_mir::CallKind, Vec<skotch_mir::LocalId>)> {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, Stmt as MStmt};
    use skotch_syntax::SyntaxKind as S;

    // Callee must be `println` or `print`.
    let name = match call.callee() {
        Some(KtExpr::Reference(r)) => r.name(),
        _ => None,
    }?;
    if name != "println" && name != "print" {
        return None;
    }

    let args = call.value_argument_list()?;
    let arg_exprs: Vec<KtExpr<'_>> = args.arguments().filter_map(|a| a.expression()).collect();
    if arg_exprs.len() != 1 {
        return None;
    }
    let KtExpr::String(_) = &arg_exprs[0] else {
        return None;
    };
    let template = &arg_exprs[0];

    // Collect parts.
    let outer_param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut part_slots: Vec<LocalId> = Vec::new();
    let mut had_interp = false;

    for child in skotch_ast::children(template.syntax()) {
        match child.kind {
            S::LITERAL_STRING_TEMPLATE_ENTRY => {
                let mut buf = String::new();
                for cc in skotch_ast::children(child) {
                    if cc.kind == S::STRING_CHUNK {
                        if let skotch_sil::SilData::Token { text } = &cc.data {
                            buf.push_str(text);
                        }
                    }
                }
                let sid = match strings.iter().position(|s| s == &buf) {
                    Some(i) => skotch_mir::StringId(i as u32),
                    None => {
                        let id = skotch_mir::StringId(strings.len() as u32);
                        strings.push(buf);
                        id
                    }
                };
                let slot = LocalId(*next_slot);
                *next_slot += 1;
                extra_locals.push(Ty::String);
                pre_stmts.push(MStmt::Assign {
                    dest: slot,
                    value: skotch_mir::Rvalue::Const(skotch_mir::MirConst::String(sid)),
                });
                part_slots.push(slot);
            }
            S::SHORT_STRING_TEMPLATE_ENTRY => {
                had_interp = true;
                // SHORT_STRING_TEMPLATE_ENTRY has children:
                //   STRING_IDENT_REF "$"
                //   REFERENCE_EXPRESSION { IDENTIFIER "name" }
                let name = skotch_ast::children(child).iter().find_map(|c| {
                    if c.kind == S::REFERENCE_EXPRESSION {
                        for cc in skotch_ast::children(c) {
                            if cc.kind == S::IDENTIFIER {
                                if let skotch_sil::SilData::Token { text } = &cc.data {
                                    return Some(text.as_str().to_string());
                                }
                            }
                        }
                    }
                    None
                })?;
                // Resolve against outer params.
                let idx = outer_param_names.iter().position(|p| p == &name)?;
                part_slots.push(LocalId(idx as u32));
            }
            S::STRING_START | S::STRING_END | S::WHITE_SPACE => {}
            S::LONG_STRING_TEMPLATE_ENTRY | S::BLOCK_STRING_TEMPLATE_ENTRY => return None,
            _ => return None,
        }
    }

    if !had_interp {
        return None;
    }

    let kind = match name {
        "println" => skotch_mir::CallKind::PrintlnConcat,
        _ => return None, // print() doesn't have a Concat variant
    };
    Some((kind, part_slots))
}

/// Try to lower `println(literal)` / `print(literal)` to the
/// Println / Print intrinsic. Returns None when the call isn't of
/// this exact shape.
fn try_lower_println_call(
    call: &skotch_ast::KtCallExpression<'_>,
    strings: &mut Vec<String>,
) -> Option<Vec<BasicBlock>> {
    use skotch_ast::KtExpr;
    // Callee must be a bare Reference named `println` or `print`.
    let name = match call.callee() {
        Some(KtExpr::Reference(r)) => r.name(),
        _ => None,
    }?;
    let (kind, is_println) = match name {
        "println" => (skotch_mir::CallKind::Println, true),
        "print" => (skotch_mir::CallKind::Print, false),
        _ => return None,
    };
    let _ = is_println;
    let args = call.value_argument_list()?;
    let arg_exprs: Vec<KtExpr<'_>> = args.arguments().filter_map(|a| a.expression()).collect();
    if arg_exprs.len() != 1 {
        return None;
    }
    let (arg_const, arg_ty) = literal_to_const(&arg_exprs[0], strings)?;
    // Layout: local 0 holds the arg, local 1 holds the unused return.
    let arg_slot = skotch_mir::LocalId(0);
    let result_slot = skotch_mir::LocalId(1);
    let blocks = vec![BasicBlock {
        stmts: vec![
            skotch_mir::Stmt::Assign {
                dest: arg_slot,
                value: skotch_mir::Rvalue::Const(arg_const),
            },
            skotch_mir::Stmt::Assign {
                dest: result_slot,
                value: skotch_mir::Rvalue::Call {
                    kind,
                    args: vec![arg_slot],
                },
            },
        ],
        terminator: Terminator::Return,
    }];
    let _ = arg_ty;
    Some(blocks)
}

/// Extract a `MirConst` from a literal-shaped `KtExpr`. Returns the
/// const plus its `Ty`. Returns `None` for non-literal expressions
/// or interpolated string templates.
fn literal_to_const(
    e: &skotch_ast::KtExpr<'_>,
    strings: &mut Vec<String>,
) -> Option<(skotch_mir::MirConst, Ty)> {
    use skotch_ast::KtExpr;
    use skotch_mir::MirConst;
    use skotch_syntax::SyntaxKind as S;
    match e {
        KtExpr::Integer(_) => {
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if c.kind == S::INTEGER_LITERAL {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            })?;
            let v: i64 = text.parse().ok()?;
            Some((MirConst::Int(v as i32), Ty::Int))
        }
        KtExpr::Boolean(_) => {
            let is_true = skotch_ast::children(e.syntax())
                .iter()
                .any(|c| c.kind == S::KW_TRUE);
            Some((MirConst::Bool(is_true), Ty::Bool))
        }
        KtExpr::Null(_) => Some((MirConst::Null, Ty::Nullable(Box::new(Ty::Any)))),
        KtExpr::String(_) => {
            let mut buf = String::new();
            let mut interpolated = false;
            for child in skotch_ast::children(e.syntax()) {
                match child.kind {
                    S::LITERAL_STRING_TEMPLATE_ENTRY => {
                        for cc in skotch_ast::children(child) {
                            if cc.kind == S::STRING_CHUNK {
                                if let skotch_sil::SilData::Token { text } = &cc.data {
                                    buf.push_str(text);
                                }
                            }
                        }
                    }
                    S::STRING_START | S::STRING_END | S::WHITE_SPACE => {}
                    _ => interpolated = true,
                }
            }
            if interpolated {
                return None;
            }
            let sid = match strings.iter().position(|s| s == &buf) {
                Some(i) => skotch_mir::StringId(i as u32),
                None => {
                    let id = skotch_mir::StringId(strings.len() as u32);
                    strings.push(buf);
                    id
                }
            };
            Some((MirConst::String(sid), Ty::String))
        }
        _ => None,
    }
}

/// Build a function body for an expression-bodied function when the
/// expression is a recognized literal. The body becomes:
///   local N: Ty
///   Assign(local N, Const(literal))
///   Return value local N
/// Block-bodied functions and non-literal expression bodies fall
/// back to an empty Return placeholder.
fn lower_simple_body(
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    fn_lookup: &rustc_hash::FxHashMap<String, (skotch_mir::FuncId, Ty)>,
    val_lookup: &rustc_hash::FxHashMap<String, Ty>,
    wrapper_class: &str,
) -> (Vec<BasicBlock>, Vec<Ty>) {
    use skotch_ast::KtExpr;
    use skotch_mir::{LocalId, MirConst};

    let make_placeholder = || {
        (
            vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            Vec::new(),
        )
    };

    // First try expression-bodied form. If absent, walk the block:
    //   - single `return <literal>`            → literal return
    //   - single `println(<literal>)` call     → Println intrinsic
    //   (block bodies with multiple statements still fall back)
    let body_expr = match f.body_expression() {
        Some(e) => e,
        None => {
            let Some(block) = f.body_block() else {
                return make_placeholder();
            };
            // Collect non-trivia statements.
            // Try `while (cond) { body }` with comparison cond + empty body.
            // Emits a 3-block CFG:
            //   block 0: cmp; Branch(then=1, exit=2)
            //   block 1: (empty body); Goto(0)
            //   block 2: Return
            if let Some(blocks_and_locals) =
                try_lower_while_loop(block, f, strings, fn_lookup)
            {
                return blocks_and_locals;
            }
            // do { body } while (cond) — body runs first.
            //   block 0: body; Goto(1)
            //   block 1: cmp; Branch(then=0, exit=2)
            //   block 2: Return
            if let Some(blocks_and_locals) =
                try_lower_do_while_loop(block, f, strings)
            {
                return blocks_and_locals;
            }

            let stmts: Vec<KtExpr<'_>> = block.statements().collect();
            // First try println("Hello, $name") template lowering.
            if stmts.len() == 1 {
                if let KtExpr::Call(call) = &stmts[0] {
                    let mut next_slot = f
                        .value_parameter_list()
                        .map(|pl| pl.parameters().count())
                        .unwrap_or(0) as u32;
                    let mut pre_stmts: Vec<skotch_mir::Stmt> = Vec::new();
                    let mut extra_locals: Vec<Ty> = Vec::new();
                    if let Some((kind, args)) = try_lower_println_template(
                        call,
                        f,
                        strings,
                        &mut next_slot,
                        &mut pre_stmts,
                        &mut extra_locals,
                    ) {
                        let result_slot = skotch_mir::LocalId(next_slot);
                        extra_locals.push(Ty::Unit);
                        pre_stmts.push(skotch_mir::Stmt::Assign {
                            dest: result_slot,
                            value: skotch_mir::Rvalue::Call { kind, args },
                        });
                        let blocks = vec![BasicBlock {
                            stmts: pre_stmts,
                            terminator: Terminator::Return,
                        }];
                        return (blocks, extra_locals);
                    }
                }
            }
            // Walk PROPERTY children + KtExpr stmts together via
            // try_lower_multi_stmt_block — this handles
            // `val x = 10; println(x)` and similar simple shapes.
            if let Some((blocks, locals)) =
                try_lower_multi_stmt_block_inner(block, f, strings, fn_lookup)
            {
                return (blocks, locals);
            }
            if stmts.len() == 1 {
                if let KtExpr::Call(call) = &stmts[0] {
                    // `println(literal)` / `print(literal)` → Println/Print intrinsic.
                    if let Some(blocks) = try_lower_println_call(call, strings) {
                        // Pull the arg's Ty from the first stmt's Const.
                        let arg_ty = blocks
                            .first()
                            .and_then(|b| b.stmts.first())
                            .and_then(|s| match s {
                                skotch_mir::Stmt::Assign { value, .. } => match value {
                                    skotch_mir::Rvalue::Const(c) => Some(const_ty(c)),
                                    _ => None,
                                },
                            })
                            .unwrap_or(Ty::Any);
                        return (blocks, vec![arg_ty, Ty::Unit]);
                    }
                }
            }
            let mut returned: Option<KtExpr<'_>> = None;
            for stmt in &stmts {
                if let KtExpr::Return(r) = stmt {
                    for c in skotch_ast::children(r.syntax()) {
                        if let Some(e) = KtExpr::cast(c) {
                            returned = Some(e);
                        }
                    }
                }
            }
            match returned {
                Some(e) => e,
                None => return make_placeholder(),
            }
        }
    };
    // Parenthesized passthrough: `(literal)` or `(a + b)`.
    let body_expr = unwrap_parens(body_expr);

    // `as` type cast: `fun toS(x: Any): String = x as String`.
    // Emits Rvalue::CheckCast with the target class descriptor.
    if let KtExpr::BinaryWithTypeRhs(b) = &body_expr {
        let children: Vec<_> = skotch_ast::children(b.syntax()).iter().collect();
        let operand = children.iter().find_map(|c| KtExpr::cast(c)).map(unwrap_parens);
        let type_name = children.iter().find_map(|c| {
            if c.kind == skotch_syntax::SyntaxKind::TYPE_REFERENCE {
                if let Some(tr) = skotch_ast::KtTypeReference::cast(c) {
                    return tr.user_type().and_then(|u| u.name()).map(String::from);
                }
            }
            None
        });
        // Operation must be `as` (KW_AS). The keyword is wrapped in
        // an OPERATION_REFERENCE composite, so check one level deep.
        let is_as = children.iter().any(|c| {
            if c.kind == skotch_syntax::SyntaxKind::OPERATION_REFERENCE {
                skotch_ast::children(c)
                    .iter()
                    .any(|cc| cc.kind == skotch_syntax::SyntaxKind::KW_AS)
            } else {
                false
            }
        });
        if is_as {
            if let (Some(KtExpr::Reference(r)), Some(tname)) = (operand, type_name) {
                if let Some(name) = r.name() {
                    let param_names: Vec<String> = f
                        .value_parameter_list()
                        .map(|pl| {
                            pl.parameters()
                                .map(|p| p.name().unwrap_or("").to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(idx) = param_names.iter().position(|p| p == name) {
                        let target_class = skotch_types::intrinsics::kotlin_to_jvm_class(&tname)
                            .map(|s| s.to_string())
                            .unwrap_or(tname.clone());
                        let ret_ty = skotch_types::ty_from_name(&tname).unwrap_or(Ty::Any);
                        let param_count = param_names.len();
                        let result_slot = skotch_mir::LocalId(param_count as u32);
                        let blocks = vec![BasicBlock {
                            stmts: vec![skotch_mir::Stmt::Assign {
                                dest: result_slot,
                                value: skotch_mir::Rvalue::CheckCast {
                                    obj: skotch_mir::LocalId(idx as u32),
                                    target_class,
                                },
                            }],
                            terminator: Terminator::ReturnValue(result_slot),
                        }];
                        return (blocks, vec![ret_ty]);
                    }
                }
            }
        }
    }

    // `is` type check: `fun isInt(x: Any): Boolean = x is Int`.
    // Emits Rvalue::InstanceOf with the param slot and the type
    // descriptor (e.g. "java/lang/Integer" for Int).
    if let KtExpr::Is(is_e) = &body_expr {
        // First child is the operand (Reference); the IS keyword and
        // the type follow.
        let children: Vec<_> = skotch_ast::children(is_e.syntax()).iter().collect();
        let operand = children.iter().find_map(|c| KtExpr::cast(c)).map(unwrap_parens);
        let type_name = children.iter().find_map(|c| {
            if c.kind == skotch_syntax::SyntaxKind::TYPE_REFERENCE {
                if let Some(tr) = skotch_ast::KtTypeReference::cast(c) {
                    return tr.user_type().and_then(|u| u.name()).map(String::from);
                }
            }
            None
        });
        if let (Some(KtExpr::Reference(r)), Some(tname)) = (operand, type_name) {
            if let Some(name) = r.name() {
                let param_names: Vec<String> = f
                    .value_parameter_list()
                    .map(|pl| {
                        pl.parameters()
                            .map(|p| p.name().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(idx) = param_names.iter().position(|p| p == name) {
                    // Boxed JVM type descriptor for primitive types
                    let descriptor =
                        skotch_types::intrinsics::kotlin_to_jvm_class(&tname)
                            .map(|s| s.to_string())
                            .unwrap_or(tname.clone());
                    let param_count = param_names.len();
                    let result_slot = skotch_mir::LocalId(param_count as u32);
                    let blocks = vec![BasicBlock {
                        stmts: vec![skotch_mir::Stmt::Assign {
                            dest: result_slot,
                            value: skotch_mir::Rvalue::InstanceOf {
                                obj: skotch_mir::LocalId(idx as u32),
                                type_descriptor: descriptor,
                            },
                        }],
                        terminator: Terminator::ReturnValue(result_slot),
                    }];
                    return (blocks, vec![Ty::Bool]);
                }
            }
        }
    }

    // Prefix unary minus on param: `fun neg(x: Int): Int = -x` →
    // BinOp(SubI, 0, x).
    if let KtExpr::Prefix(p) = &body_expr {
        let op_text = skotch_ast::children(p.syntax())
            .iter()
            .find_map(|c| {
                if c.kind == skotch_syntax::SyntaxKind::OPERATION_REFERENCE {
                    if let Some(opref) = skotch_ast::KtOperationReference::cast(c) {
                        return Some(opref.text());
                    }
                }
                None
            })
            .unwrap_or_default();
        if op_text == "-" {
            let inner = skotch_ast::children(p.syntax())
                .iter()
                .find_map(KtExpr::cast)
                .map(unwrap_parens);
            if let Some(KtExpr::Reference(r)) = inner {
                if let Some(name) = r.name() {
                    let param_names: Vec<String> = f
                        .value_parameter_list()
                        .map(|pl| {
                            pl.parameters()
                                .map(|p| p.name().unwrap_or("").to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(idx) = param_names.iter().position(|p| p == name) {
                        let param_slot = skotch_mir::LocalId(idx as u32);
                        let param_count = param_names.len();
                        let zero_slot = skotch_mir::LocalId(param_count as u32);
                        let result_slot = skotch_mir::LocalId((param_count + 1) as u32);
                        let blocks = vec![BasicBlock {
                            stmts: vec![
                                skotch_mir::Stmt::Assign {
                                    dest: zero_slot,
                                    value: skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(0)),
                                },
                                skotch_mir::Stmt::Assign {
                                    dest: result_slot,
                                    value: skotch_mir::Rvalue::BinOp {
                                        op: skotch_mir::BinOp::SubI,
                                        lhs: zero_slot,
                                        rhs: param_slot,
                                    },
                                },
                            ],
                            terminator: Terminator::ReturnValue(result_slot),
                        }];
                        return (blocks, vec![Ty::Int, Ty::Int]);
                    }
                }
            }
        }
    }

    // Throw expression body:
    //   fun fail(): Nothing = throw e
    // Where the thrown value is a Reference to a parameter, the
    // body becomes:
    //   block 0: Terminator::Throw(param_slot)
    if let KtExpr::Throw(t) = &body_expr {
        // Find the inner KtExpr (the thrown value).
        let thrown = skotch_ast::children(t.syntax())
            .iter()
            .find_map(KtExpr::cast)
            .map(unwrap_parens);
        if let Some(KtExpr::Reference(r)) = thrown {
            if let Some(name) = r.name() {
                let param_names: Vec<String> = f
                    .value_parameter_list()
                    .map(|pl| {
                        pl.parameters()
                            .map(|p| p.name().unwrap_or("").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(idx) = param_names.iter().position(|p| p == name) {
                    let blocks = vec![BasicBlock {
                        stmts: Vec::new(),
                        terminator: Terminator::Throw(skotch_mir::LocalId(idx as u32)),
                    }];
                    return (blocks, Vec::new());
                }
            }
        }
    }

    // when (subject) { v1 -> result1; v2 -> result2; else -> default }
    // expression body. Lowers to a chain of comparison blocks.
    if let KtExpr::When(w) = &body_expr {
        if let Some(lowered) = try_lower_when_expression(w, f, strings) {
            return lowered;
        }
    }

    // if/else expression body:
    //   fun max(a: Int, b: Int): Int = if (a > b) a else b
    // Emits a 4-block CFG:
    //   block 0: cond_local = BinOp(cond); Branch(cond_local, 1, 2)
    //   block 1: result_local = then-arm; Goto(3)
    //   block 2: result_local = else-arm; Goto(3)
    //   block 3: ReturnValue(result_local)
    if let KtExpr::If(if_e) = &body_expr {
        if let Some(blocks_and_locals) = try_lower_if_expression(if_e, f, strings, fn_lookup) {
            return blocks_and_locals;
        }
    }

    // Static-call body: `fun outer() = inner(arg1, arg2)` where
    // inner is a top-level fn in the same file. Args may be either
    // literal constants or References to the outer's parameters.
    if let KtExpr::Call(call) = &body_expr {
        if let Some(KtExpr::Reference(r)) = call.callee() {
            if let Some(name) = r.name() {
                if let Some((callee_id, callee_ret)) = fn_lookup.get(name) {
                    let param_count = f
                        .value_parameter_list()
                        .map(|pl| pl.parameters().count())
                        .unwrap_or(0);
                    let outer_param_names: Vec<String> = f
                        .value_parameter_list()
                        .map(|pl| {
                            pl.parameters()
                                .map(|p| p.name().unwrap_or("").to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut next_slot = param_count as u32;
                    let mut pre_stmts: Vec<skotch_mir::Stmt> = Vec::new();
                    let mut extra_locals: Vec<Ty> = Vec::new();
                    let mut arg_slots: Vec<skotch_mir::LocalId> = Vec::new();
                    let mut ok = true;
                    if let Some(arg_list) = call.value_argument_list() {
                        for arg in arg_list.arguments() {
                            let Some(arg_expr) = arg.expression() else {
                                ok = false;
                                break;
                            };
                            match arg_expr {
                                KtExpr::Reference(rr) => {
                                    let Some(an) = rr.name() else {
                                        ok = false;
                                        break;
                                    };
                                    let Some(idx) = outer_param_names.iter().position(|p| p == an)
                                    else {
                                        ok = false;
                                        break;
                                    };
                                    arg_slots.push(skotch_mir::LocalId(idx as u32));
                                }
                                other => match literal_to_const(&other, strings) {
                                    Some((k, ty)) => {
                                        let slot = skotch_mir::LocalId(next_slot);
                                        next_slot += 1;
                                        extra_locals.push(ty);
                                        pre_stmts.push(skotch_mir::Stmt::Assign {
                                            dest: slot,
                                            value: skotch_mir::Rvalue::Const(k),
                                        });
                                        arg_slots.push(slot);
                                    }
                                    None => {
                                        ok = false;
                                        break;
                                    }
                                },
                            }
                        }
                    }
                    if ok {
                        let result_slot = skotch_mir::LocalId(next_slot);
                        extra_locals.push(callee_ret.clone());
                        pre_stmts.push(skotch_mir::Stmt::Assign {
                            dest: result_slot,
                            value: skotch_mir::Rvalue::Call {
                                kind: skotch_mir::CallKind::Static(*callee_id),
                                args: arg_slots,
                            },
                        });
                        let blocks = vec![BasicBlock {
                            stmts: pre_stmts,
                            terminator: if callee_ret == &Ty::Unit {
                                Terminator::Return
                            } else {
                                Terminator::ReturnValue(result_slot)
                            },
                        }];
                        return (blocks, extra_locals);
                    }
                }
            }
        }
    }

    // Identity function body: `fun id(x: Int): Int = x` returns the
    // parameter directly with no intermediate slot. Just ReturnValue
    // on the param's LocalId.
    if let KtExpr::Reference(r) = &body_expr {
        if let Some(name) = r.name() {
            let param_names: Vec<String> = f
                .value_parameter_list()
                .map(|pl| {
                    pl.parameters()
                        .map(|p| p.name().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(idx) = param_names.iter().position(|p| p == name) {
                let blocks = vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::ReturnValue(skotch_mir::LocalId(idx as u32)),
                }];
                return (blocks, Vec::new());
            }
            // Top-level val reference: emit GetStaticField on the
            // wrapper class.
            if let Some(val_ty) = val_lookup.get(name) {
                let param_count = param_names.len();
                let result_slot = skotch_mir::LocalId(param_count as u32);
                let descriptor = ty_to_descriptor(val_ty);
                let blocks = vec![BasicBlock {
                    stmts: vec![skotch_mir::Stmt::Assign {
                        dest: result_slot,
                        value: skotch_mir::Rvalue::GetStaticField {
                            class_name: wrapper_class.to_string(),
                            field_name: name.to_string(),
                            descriptor,
                        },
                    }],
                    terminator: Terminator::ReturnValue(result_slot),
                }];
                return (blocks, vec![val_ty.clone()]);
            }
        }
    }

    // Binary arithmetic body where each operand is either a param
    // reference or a literal constant. Examples:
    //   fun add(a: Int, b: Int) = a + b
    //   fun double(x: Int) = x * 2
    //   fun addOne(x: Int) = x + 1
    if let KtExpr::Binary(b) = &body_expr {
        let param_count = f
            .value_parameter_list()
            .map(|pl| pl.parameters().count())
            .unwrap_or(0);
        let param_names: Vec<String> = f
            .value_parameter_list()
            .map(|pl| {
                pl.parameters()
                    .map(|p| p.name().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        let op_text = b.operation().map(|o| o.text()).unwrap_or_default();
        // Detect the dominant numeric Ty among operands (String wins
        // for `+` via ConcatStr; otherwise Long/Float/Double bump
        // the variant from AddI to AddL/AddF/AddD).
        let is_str_concat = op_text == "+" && {
            let lhs_str = b.lhs().is_some_and(|l| operand_is_string(&l, f));
            let rhs_str = b.rhs().is_some_and(|r| operand_is_string(&r, f));
            lhs_str || rhs_str
        };
        let numeric_ty = if is_str_concat {
            Ty::String
        } else {
            let lhs_ty = b
                .lhs()
                .map(|l| operand_numeric_ty(&l, f))
                .unwrap_or(Ty::Int);
            let rhs_ty = b
                .rhs()
                .map(|r| operand_numeric_ty(&r, f))
                .unwrap_or(Ty::Int);
            promote_numeric(&lhs_ty, &rhs_ty)
        };
        let mir_op = match (op_text.as_str(), &numeric_ty) {
            ("+", Ty::String) => Some(skotch_mir::BinOp::ConcatStr),
            ("+", Ty::Long) => Some(skotch_mir::BinOp::AddL),
            ("-", Ty::Long) => Some(skotch_mir::BinOp::SubL),
            ("*", Ty::Long) => Some(skotch_mir::BinOp::MulL),
            ("/", Ty::Long) => Some(skotch_mir::BinOp::DivL),
            ("%", Ty::Long) => Some(skotch_mir::BinOp::ModL),
            ("+", Ty::Double) => Some(skotch_mir::BinOp::AddD),
            ("-", Ty::Double) => Some(skotch_mir::BinOp::SubD),
            ("*", Ty::Double) => Some(skotch_mir::BinOp::MulD),
            ("/", Ty::Double) => Some(skotch_mir::BinOp::DivD),
            ("%", Ty::Double) => Some(skotch_mir::BinOp::ModD),
            ("+", Ty::Float) => Some(skotch_mir::BinOp::AddF),
            ("-", Ty::Float) => Some(skotch_mir::BinOp::SubF),
            ("*", Ty::Float) => Some(skotch_mir::BinOp::MulF),
            ("/", Ty::Float) => Some(skotch_mir::BinOp::DivF),
            ("%", Ty::Float) => Some(skotch_mir::BinOp::ModF),
            ("+", _) => Some(skotch_mir::BinOp::AddI),
            ("-", _) => Some(skotch_mir::BinOp::SubI),
            ("*", _) => Some(skotch_mir::BinOp::MulI),
            ("/", _) => Some(skotch_mir::BinOp::DivI),
            ("%", _) => Some(skotch_mir::BinOp::ModI),
            ("==", _) => Some(skotch_mir::BinOp::CmpEq),
            ("!=", _) => Some(skotch_mir::BinOp::CmpNe),
            ("<", _) => Some(skotch_mir::BinOp::CmpLt),
            (">", _) => Some(skotch_mir::BinOp::CmpGt),
            ("<=", _) => Some(skotch_mir::BinOp::CmpLe),
            (">=", _) => Some(skotch_mir::BinOp::CmpGe),
            _ => None,
        };
        if let Some(op) = mir_op {
            // Pre-allocate slots: params (locals 0..N), then
            // optional Const slots for each literal operand, then
            // the result slot.
            let mut next_slot = param_count as u32;
            let mut pre_stmts: Vec<skotch_mir::Stmt> = Vec::new();
            let mut extra_locals: Vec<Ty> = Vec::new();

            // resolve_operand handles Reference / literal / nested
            // Binary. Nested Binary recurses to emit the inner
            // BinOp into its own slot, then returns that slot.
            fn resolve_operand_rec(
                e: skotch_ast::KtExpr<'_>,
                f: skotch_ast::KtFun<'_>,
                param_names: &[String],
                next_slot: &mut u32,
                pre_stmts: &mut Vec<skotch_mir::Stmt>,
                extra_locals: &mut Vec<Ty>,
                strings: &mut Vec<String>,
            ) -> Option<skotch_mir::LocalId> {
                use skotch_ast::KtExpr;
                let e = unwrap_parens(e);
                match e {
                    KtExpr::Reference(r) => {
                        let n = r.name()?;
                        let idx = param_names.iter().position(|p| p == n)?;
                        Some(skotch_mir::LocalId(idx as u32))
                    }
                    KtExpr::Binary(inner_b) => {
                        // Recurse: lower the inner binary into its own slot.
                        let inner_lhs = resolve_operand_rec(
                            inner_b.lhs()?,
                            f,
                            param_names,
                            next_slot,
                            pre_stmts,
                            extra_locals,
                            strings,
                        )?;
                        let inner_rhs = resolve_operand_rec(
                            inner_b.rhs()?,
                            f,
                            param_names,
                            next_slot,
                            pre_stmts,
                            extra_locals,
                            strings,
                        )?;
                        let op_text = inner_b.operation().map(|o| o.text()).unwrap_or_default();
                        let lhs_ty = operand_numeric_ty(&inner_b.lhs()?, f);
                        let rhs_ty = operand_numeric_ty(&inner_b.rhs()?, f);
                        let ty = promote_numeric(&lhs_ty, &rhs_ty);
                        let mir_op = match (op_text.as_str(), &ty) {
                            ("+", Ty::Long) => Some(skotch_mir::BinOp::AddL),
                            ("-", Ty::Long) => Some(skotch_mir::BinOp::SubL),
                            ("*", Ty::Long) => Some(skotch_mir::BinOp::MulL),
                            ("/", Ty::Long) => Some(skotch_mir::BinOp::DivL),
                            ("%", Ty::Long) => Some(skotch_mir::BinOp::ModL),
                            ("+", Ty::Double) => Some(skotch_mir::BinOp::AddD),
                            ("-", Ty::Double) => Some(skotch_mir::BinOp::SubD),
                            ("*", Ty::Double) => Some(skotch_mir::BinOp::MulD),
                            ("/", Ty::Double) => Some(skotch_mir::BinOp::DivD),
                            ("%", Ty::Double) => Some(skotch_mir::BinOp::ModD),
                            ("+", Ty::Float) => Some(skotch_mir::BinOp::AddF),
                            ("-", Ty::Float) => Some(skotch_mir::BinOp::SubF),
                            ("*", Ty::Float) => Some(skotch_mir::BinOp::MulF),
                            ("/", Ty::Float) => Some(skotch_mir::BinOp::DivF),
                            ("%", Ty::Float) => Some(skotch_mir::BinOp::ModF),
                            ("+", _) => Some(skotch_mir::BinOp::AddI),
                            ("-", _) => Some(skotch_mir::BinOp::SubI),
                            ("*", _) => Some(skotch_mir::BinOp::MulI),
                            ("/", _) => Some(skotch_mir::BinOp::DivI),
                            ("%", _) => Some(skotch_mir::BinOp::ModI),
                            _ => None,
                        }?;
                        let slot = skotch_mir::LocalId(*next_slot);
                        *next_slot += 1;
                        extra_locals.push(ty);
                        pre_stmts.push(skotch_mir::Stmt::Assign {
                            dest: slot,
                            value: skotch_mir::Rvalue::BinOp {
                                op: mir_op,
                                lhs: inner_lhs,
                                rhs: inner_rhs,
                            },
                        });
                        Some(slot)
                    }
                    other => {
                        let (k, ty) = literal_to_const(&other, strings)?;
                        let slot = skotch_mir::LocalId(*next_slot);
                        *next_slot += 1;
                        extra_locals.push(ty);
                        pre_stmts.push(skotch_mir::Stmt::Assign {
                            dest: slot,
                            value: skotch_mir::Rvalue::Const(k),
                        });
                        Some(slot)
                    }
                }
            }

            let mut resolve_operand = |e: skotch_ast::KtExpr<'_>,
                                       next_slot: &mut u32,
                                       pre_stmts: &mut Vec<skotch_mir::Stmt>,
                                       extra_locals: &mut Vec<Ty>|
             -> Option<skotch_mir::LocalId> {
                resolve_operand_rec(
                    e,
                    f,
                    &param_names,
                    next_slot,
                    pre_stmts,
                    extra_locals,
                    strings,
                )
            };

            let lhs_slot = b.lhs().and_then(|l| {
                resolve_operand(l, &mut next_slot, &mut pre_stmts, &mut extra_locals)
            });
            let rhs_slot = b.rhs().and_then(|r| {
                resolve_operand(r, &mut next_slot, &mut pre_stmts, &mut extra_locals)
            });
            if let (Some(lhs), Some(rhs)) = (lhs_slot, rhs_slot) {
                let is_cmp = matches!(
                    op,
                    skotch_mir::BinOp::CmpEq
                        | skotch_mir::BinOp::CmpNe
                        | skotch_mir::BinOp::CmpLt
                        | skotch_mir::BinOp::CmpGt
                        | skotch_mir::BinOp::CmpLe
                        | skotch_mir::BinOp::CmpGe
                );
                let return_ty = if is_cmp {
                    Ty::Bool
                } else {
                    // Prefer the promoted numeric_ty when it's a
                    // concrete numeric / String; fall back to the
                    // function's declared return type otherwise.
                    match &numeric_ty {
                        Ty::Int | Ty::Long | Ty::Float | Ty::Double | Ty::String => {
                            numeric_ty.clone()
                        }
                        _ => match f
                            .return_type()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                        {
                            Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Int),
                            None => Ty::Int,
                        },
                    }
                };
                let result_slot = skotch_mir::LocalId(next_slot);
                extra_locals.push(return_ty);
                pre_stmts.push(skotch_mir::Stmt::Assign {
                    dest: result_slot,
                    value: skotch_mir::Rvalue::BinOp { op, lhs, rhs },
                });
                let blocks = vec![BasicBlock {
                    stmts: pre_stmts,
                    terminator: Terminator::ReturnValue(result_slot),
                }];
                return (blocks, extra_locals);
            }
        }
    }
    let (c, ty) = match &body_expr {
        KtExpr::Integer(_) => {
            let text = skotch_ast::children(body_expr.syntax())
                .iter()
                .find_map(|cc| {
                    if cc.kind == skotch_syntax::SyntaxKind::INTEGER_LITERAL {
                        if let skotch_sil::SilData::Token { text } = &cc.data {
                            return Some(text.as_str());
                        }
                    }
                    None
                });
            let Some(text) = text else {
                return make_placeholder();
            };
            let Ok(v) = text.parse::<i64>() else {
                return make_placeholder();
            };
            (MirConst::Int(v as i32), Ty::Int)
        }
        KtExpr::Boolean(_) => {
            let is_true = skotch_ast::children(body_expr.syntax())
                .iter()
                .any(|cc| cc.kind == skotch_syntax::SyntaxKind::KW_TRUE);
            (MirConst::Bool(is_true), Ty::Bool)
        }
        KtExpr::Null(_) => (MirConst::Null, Ty::Nullable(Box::new(Ty::Any))),
        KtExpr::String(_) => {
            // Build the string from the LITERAL_STRING_TEMPLATE_ENTRY
            // children: a plain literal with no $ interpolation has
            // exactly one entry whose child is a STRING_CHUNK token.
            let mut buf = String::new();
            let mut interpolated = false;
            for child in skotch_ast::children(body_expr.syntax()) {
                use skotch_syntax::SyntaxKind as S;
                match child.kind {
                    S::LITERAL_STRING_TEMPLATE_ENTRY => {
                        for cc in skotch_ast::children(child) {
                            if cc.kind == S::STRING_CHUNK {
                                if let skotch_sil::SilData::Token { text } = &cc.data {
                                    buf.push_str(text);
                                }
                            }
                        }
                    }
                    S::STRING_START | S::STRING_END | S::WHITE_SPACE => {}
                    _ => {
                        interpolated = true;
                    }
                }
            }
            if interpolated {
                return make_placeholder();
            }
            let sid = match strings.iter().position(|s| s == &buf) {
                Some(i) => skotch_mir::StringId(i as u32),
                None => {
                    let id = skotch_mir::StringId(strings.len() as u32);
                    strings.push(buf);
                    id
                }
            };
            (MirConst::String(sid), Ty::String)
        }
        _ => return make_placeholder(),
    };

    // Decide the result local slot. With no params, the result lives
    // in local 0; otherwise it's the next slot after the params.
    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let result_slot = LocalId(param_count as u32);
    let extra_locals = vec![ty];
    let blocks = vec![BasicBlock {
        stmts: vec![skotch_mir::Stmt::Assign {
            dest: result_slot,
            value: skotch_mir::Rvalue::Const(c),
        }],
        terminator: Terminator::ReturnValue(result_slot),
    }];
    (blocks, extra_locals)
}

/// Build an `<init>(P1, P2, ...)V` constructor from a class's
/// primary-constructor parameter list. Parameter types come from
/// each KtValueParameter's KtTypeReference (Ty::Any when missing).
/// Body is an empty Return for now; field-init writebacks are a
/// follow-up.
fn constructor_from_primary(c: skotch_ast::KtClass<'_>, class_name: &str) -> MirFunction {
    let Some(pc) = c.primary_constructor() else {
        return empty_constructor(class_name);
    };
    let plist = match pc.value_parameter_list() {
        Some(pl) => pl,
        None => return empty_constructor(class_name),
    };
    let params_iter: Vec<_> = plist.parameters().collect();
    let param_count = params_iter.len();
    let param_names: Vec<String> = params_iter
        .iter()
        .map(|p| p.name().unwrap_or("").to_string())
        .collect();
    let param_tys: Vec<Ty> = params_iter
        .iter()
        .map(|p| {
            p.type_reference()
                .and_then(|tr| tr.user_type())
                .and_then(|u| u.name())
                .and_then(skotch_types::ty_from_name)
                .unwrap_or(Ty::Any)
        })
        .collect();
    let params: Vec<skotch_mir::LocalId> = (0..param_count)
        .map(|i| skotch_mir::LocalId(i as u32))
        .collect();
    MirFunction {
        id: FuncId(0),
        name: "<init>".to_string(),
        params,
        locals: param_tys,
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::Unit,
        required_params: param_count,
        param_names,
        param_receiver_types: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract: false,
        vararg_index: None,
        exception_handlers: Vec::new(),
        is_suspend: false,
        is_inline: false,
        has_type_params: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
        named_locals: Vec::new(),
        is_private: false,
        is_static: false,
        default_call_masks: Vec::new(),
        needs_leading_nop: false,
        local_generic_args: rustc_hash::FxHashMap::default(),
    }
}

/// Collect fields from a class body's `val`/`var` properties +
/// primary-constructor `val`/`var` parameters.
fn collect_class_fields(c: skotch_ast::KtClass<'_>) -> Vec<skotch_mir::MirField> {
    let mut fields = Vec::new();
    if let Some(pc) = c.primary_constructor() {
        if let Some(plist) = pc.value_parameter_list() {
            for p in plist.parameters() {
                if p.is_val() || p.is_var() {
                    if let Some(n) = p.name() {
                        let ty = match p
                            .type_reference()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                        {
                            Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Any),
                            None => Ty::Any,
                        };
                        fields.push(skotch_mir::MirField {
                            name: n.to_string(),
                            ty,
                            is_jvm_field: false,
                        });
                    }
                }
            }
        }
    }
    if let Some(body) = c.body() {
        for d in body.declarations() {
            if let KtDecl::Property(p) = d {
                if let Some(n) = p.name() {
                    let ty = match p
                        .type_reference()
                        .and_then(|tr| tr.user_type())
                        .and_then(|u| u.name())
                    {
                        Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Any),
                        None => Ty::Any,
                    };
                    fields.push(skotch_mir::MirField {
                        name: n.to_string(),
                        ty,
                        is_jvm_field: false,
                    });
                }
            }
        }
    }
    fields
}

/// Collect secondary constructors from a class body. Each becomes a
/// MirFunction named `<init>` with empty body — full body lowering
/// (including the `: this(args)` / `: super(args)` delegation
/// emission) lands in a follow-up.
fn collect_secondary_ctors(c: skotch_ast::KtClass<'_>) -> Vec<MirFunction> {
    let mut out = Vec::new();
    let Some(body) = c.body() else { return out };
    for (sc_idx, sc) in body.secondary_constructors().enumerate() {
        let sc_idx = sc_idx as u32;
        let param_count = sc
            .value_parameter_list()
            .map(|pl| pl.parameters().count())
            .unwrap_or(0);
        let params: Vec<skotch_mir::LocalId> = (0..param_count)
            .map(|i| skotch_mir::LocalId(i as u32))
            .collect();
        let param_names: Vec<String> = sc
            .value_parameter_list()
            .map(|pl| {
                pl.parameters()
                    .map(|p| p.name().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        let param_tys: Vec<Ty> = sc
            .value_parameter_list()
            .map(|pl| {
                pl.parameters()
                    .map(|p| {
                        p.type_reference()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                            .and_then(skotch_types::ty_from_name)
                            .unwrap_or(Ty::Any)
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(MirFunction {
            id: FuncId(sc_idx),
            name: "<init>".to_string(),
            params,
            locals: param_tys,
            blocks: vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            return_ty: Ty::Unit,
            required_params: param_count,
            param_names,
            param_receiver_types: Vec::new(),
            param_defaults: Vec::new(),
            is_abstract: false,
            vararg_index: None,
            exception_handlers: Vec::new(),
            is_suspend: false,
            is_inline: false,
            has_type_params: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: Vec::new(),
            named_locals: Vec::new(),
            is_private: false,
            is_static: false,
            default_call_masks: Vec::new(),
            needs_leading_nop: false,
            local_generic_args: rustc_hash::FxHashMap::default(),
        });
    }
    out
}

/// Same-shape helper for interfaces.
fn collect_interface_methods(
    i: skotch_ast::KtInterface<'_>,
    strings: &mut Vec<String>,
) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = i.body() else { return methods };
    let mut method_idx = 0u32;
    for d in body.declarations() {
        if let KtDecl::Fun(f) = d {
            methods.push(method_from_fun(f, method_idx, true, strings));
            method_idx += 1;
        }
    }
    methods
}

/// Same-shape helper for object singletons.
fn collect_object_methods(
    o: skotch_ast::KtObjectDeclaration<'_>,
    strings: &mut Vec<String>,
) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = o.body() else { return methods };
    let mut method_idx = 0u32;
    for d in body.declarations() {
        if let KtDecl::Fun(f) = d {
            methods.push(method_from_fun(f, method_idx, false, strings));
            method_idx += 1;
        }
    }
    methods
}

/// Lower a class/interface method body when it has a recognizable
/// shape. The receiver `this` is at slot 0; user params at 1..N+1.
/// `class_name` and `field_names` are needed for `Reference(field)`
/// in the body to emit GetField on `this`.
fn method_simple_body_with_class(
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
    class_name: Option<&str>,
    field_names: &[(String, Ty)],
) -> (Vec<BasicBlock>, Vec<Ty>) {
    use skotch_ast::KtExpr;

    let make_placeholder = || {
        (
            vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            Vec::new(),
        )
    };

    let body_expr = match f.body_expression() {
        Some(e) => e,
        None => {
            let Some(block) = f.body_block() else {
                return make_placeholder();
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
            match returned {
                Some(e) => e,
                None => return make_placeholder(),
            }
        }
    };
    let body_expr = unwrap_parens(body_expr);

    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    // `this` reference body: `fun self(): T = this` returns slot 0.
    if let KtExpr::This(_) = &body_expr {
        let blocks = vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::ReturnValue(skotch_mir::LocalId(0)),
        }];
        return (blocks, Vec::new());
    }

    // Identity-ref body: `fun id(x: Int): Int = x` returns the
    // parameter slot directly (1-indexed past `this`).
    if let KtExpr::Reference(r) = &body_expr {
        if let Some(name) = r.name() {
            if let Some(idx) = param_names.iter().position(|p| p == name) {
                let param_slot = skotch_mir::LocalId((1 + idx) as u32);
                let blocks = vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::ReturnValue(param_slot),
                }];
                return (blocks, Vec::new());
            }
            // Field access via implicit this: `fun get() = x` where
            // x is a primary-ctor val/var on the enclosing class.
            if let (Some(cname), Some((fname, fty))) = (
                class_name,
                field_names.iter().find(|(n, _)| n == name),
            ) {
                let this_slot = skotch_mir::LocalId(0);
                let result_slot = skotch_mir::LocalId((1 + param_count) as u32);
                let blocks = vec![BasicBlock {
                    stmts: vec![skotch_mir::Stmt::Assign {
                        dest: result_slot,
                        value: skotch_mir::Rvalue::GetField {
                            receiver: this_slot,
                            class_name: cname.to_string(),
                            field_name: fname.clone(),
                        },
                    }],
                    terminator: Terminator::ReturnValue(result_slot),
                }];
                return (blocks, vec![fty.clone()]);
            }
        }
    }

    // Binary op on param/field refs or literals:
    //   fun add(a: Int, b: Int) = a + b
    //   fun double() = x * 2      (x is a field)
    //   fun bump() = x + 1
    if let KtExpr::Binary(b) = &body_expr {
        let op_text = b.operation().map(|o| o.text()).unwrap_or_default();
        let mir_op = match op_text.as_str() {
            "+" => Some(skotch_mir::BinOp::AddI),
            "-" => Some(skotch_mir::BinOp::SubI),
            "*" => Some(skotch_mir::BinOp::MulI),
            "/" => Some(skotch_mir::BinOp::DivI),
            "%" => Some(skotch_mir::BinOp::ModI),
            "==" => Some(skotch_mir::BinOp::CmpEq),
            "!=" => Some(skotch_mir::BinOp::CmpNe),
            "<" => Some(skotch_mir::BinOp::CmpLt),
            ">" => Some(skotch_mir::BinOp::CmpGt),
            "<=" => Some(skotch_mir::BinOp::CmpLe),
            ">=" => Some(skotch_mir::BinOp::CmpGe),
            _ => None,
        };
        if let Some(op) = mir_op {
            let mut next_slot = (1 + param_count) as u32;
            let mut pre_stmts: Vec<skotch_mir::Stmt> = Vec::new();
            let mut extra_locals: Vec<Ty> = Vec::new();

            let resolve = |e: KtExpr<'_>,
                           next_slot: &mut u32,
                           pre: &mut Vec<skotch_mir::Stmt>,
                           locals: &mut Vec<Ty>,
                           strings: &mut Vec<String>|
             -> Option<skotch_mir::LocalId> {
                let e = unwrap_parens(e);
                match e {
                    KtExpr::Reference(rr) => {
                        let n = rr.name()?;
                        if let Some(idx) = param_names.iter().position(|p| p == n) {
                            return Some(skotch_mir::LocalId((1 + idx) as u32));
                        }
                        // Field via implicit this.
                        if let (Some(cname), Some((fname, fty))) = (
                            class_name,
                            field_names.iter().find(|(n2, _)| n2 == n),
                        ) {
                            let slot = skotch_mir::LocalId(*next_slot);
                            *next_slot += 1;
                            locals.push(fty.clone());
                            pre.push(skotch_mir::Stmt::Assign {
                                dest: slot,
                                value: skotch_mir::Rvalue::GetField {
                                    receiver: skotch_mir::LocalId(0),
                                    class_name: cname.to_string(),
                                    field_name: fname.clone(),
                                },
                            });
                            return Some(slot);
                        }
                        None
                    }
                    other => {
                        let (k, ty) = literal_to_const(&other, strings)?;
                        let slot = skotch_mir::LocalId(*next_slot);
                        *next_slot += 1;
                        locals.push(ty);
                        pre.push(skotch_mir::Stmt::Assign {
                            dest: slot,
                            value: skotch_mir::Rvalue::Const(k),
                        });
                        Some(slot)
                    }
                }
            };
            let lhs_slot = b.lhs().and_then(|l| {
                resolve(l, &mut next_slot, &mut pre_stmts, &mut extra_locals, strings)
            });
            let rhs_slot = b.rhs().and_then(|r| {
                resolve(r, &mut next_slot, &mut pre_stmts, &mut extra_locals, strings)
            });
            if let (Some(lhs), Some(rhs)) = (lhs_slot, rhs_slot) {
                let is_cmp = matches!(
                    op,
                    skotch_mir::BinOp::CmpEq
                        | skotch_mir::BinOp::CmpNe
                        | skotch_mir::BinOp::CmpLt
                        | skotch_mir::BinOp::CmpGt
                        | skotch_mir::BinOp::CmpLe
                        | skotch_mir::BinOp::CmpGe
                );
                let result_ty = if is_cmp { Ty::Bool } else { Ty::Int };
                let result_slot = skotch_mir::LocalId(next_slot);
                extra_locals.push(result_ty);
                pre_stmts.push(skotch_mir::Stmt::Assign {
                    dest: result_slot,
                    value: skotch_mir::Rvalue::BinOp { op, lhs, rhs },
                });
                let blocks = vec![BasicBlock {
                    stmts: pre_stmts,
                    terminator: Terminator::ReturnValue(result_slot),
                }];
                return (blocks, extra_locals);
            }
        }
    }

    // throw <param-ref> body for methods:
    //   class X { fun fail(e: Throwable): Nothing = throw e }
    if let KtExpr::Throw(t) = &body_expr {
        let thrown = skotch_ast::children(t.syntax())
            .iter()
            .find_map(KtExpr::cast)
            .map(unwrap_parens);
        if let Some(KtExpr::Reference(r)) = thrown {
            if let Some(name) = r.name() {
                if let Some(idx) = param_names.iter().position(|p| p == name) {
                    let param_slot = skotch_mir::LocalId((1 + idx) as u32);
                    let blocks = vec![BasicBlock {
                        stmts: Vec::new(),
                        terminator: Terminator::Throw(param_slot),
                    }];
                    return (blocks, Vec::new());
                }
            }
        }
    }

    // Virtual call on `this` to a sibling no-arg method:
    //   class P { fun a() = 1; fun b() = a() }
    // Emits Call(Virtual { class, method: "a" }, [this]).
    if let KtExpr::Call(call) = &body_expr {
        if let Some(KtExpr::Reference(r)) = call.callee() {
            if let Some(name) = r.name() {
                let no_args = call
                    .value_argument_list()
                    .map(|a| a.arguments().count() == 0)
                    .unwrap_or(true);
                if no_args && name != "println" && name != "print" {
                    if let Some(cname) = class_name {
                        let this_slot = skotch_mir::LocalId(0);
                        let result_slot = skotch_mir::LocalId((1 + param_count) as u32);
                        // Determine return type from f.return_type when present.
                        let ret_ty = match f
                            .return_type()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                        {
                            Some(rn) => skotch_types::ty_from_name(rn).unwrap_or(Ty::Any),
                            None => Ty::Any,
                        };
                        let blocks = vec![BasicBlock {
                            stmts: vec![skotch_mir::Stmt::Assign {
                                dest: result_slot,
                                value: skotch_mir::Rvalue::Call {
                                    kind: skotch_mir::CallKind::Virtual {
                                        class_name: cname.to_string(),
                                        method_name: name.to_string(),
                                    },
                                    args: vec![this_slot],
                                },
                            }],
                            terminator: if ret_ty == Ty::Unit {
                                Terminator::Return
                            } else {
                                Terminator::ReturnValue(result_slot)
                            },
                        }];
                        return (blocks, vec![ret_ty]);
                    }
                }
            }
        }
    }

    // println(literal) / print(literal) call body for methods (often
    // appears as `fun show() = println("hi")`).
    if let KtExpr::Call(call) = &body_expr {
        if let Some(KtExpr::Reference(r)) = call.callee() {
            if let Some(name) = r.name() {
                if name == "println" || name == "print" {
                    let kind = if name == "println" {
                        skotch_mir::CallKind::Println
                    } else {
                        skotch_mir::CallKind::Print
                    };
                    if let Some(args) = call.value_argument_list() {
                        let arg_exprs: Vec<KtExpr<'_>> =
                            args.arguments().filter_map(|a| a.expression()).collect();
                        if arg_exprs.len() == 1 {
                            if let Some((k, ty)) = literal_to_const(&arg_exprs[0], strings) {
                                let arg_slot =
                                    skotch_mir::LocalId((1 + param_count) as u32);
                                let result_slot =
                                    skotch_mir::LocalId((1 + param_count + 1) as u32);
                                let blocks = vec![BasicBlock {
                                    stmts: vec![
                                        skotch_mir::Stmt::Assign {
                                            dest: arg_slot,
                                            value: skotch_mir::Rvalue::Const(k),
                                        },
                                        skotch_mir::Stmt::Assign {
                                            dest: result_slot,
                                            value: skotch_mir::Rvalue::Call {
                                                kind,
                                                args: vec![arg_slot],
                                            },
                                        },
                                    ],
                                    terminator: Terminator::Return,
                                }];
                                return (blocks, vec![ty, Ty::Unit]);
                            }
                        }
                    }
                }
            }
        }
    }

    let Some((c, ty)) = literal_to_const(&body_expr, strings) else {
        return make_placeholder();
    };

    // Slot layout for class methods:
    //   local 0: `this`
    //   locals 1..N+1: user params
    //   local N+2: result
    let result_slot = skotch_mir::LocalId((1 + param_count) as u32);
    let blocks = vec![BasicBlock {
        stmts: vec![skotch_mir::Stmt::Assign {
            dest: result_slot,
            value: skotch_mir::Rvalue::Const(c),
        }],
        terminator: Terminator::ReturnValue(result_slot),
    }];
    (blocks, vec![ty])
}

/// Build a MirFunction from a typed KtFun. `is_abstract_default`
/// applies when the source has no body and the surrounding decl is
/// an interface (where methods default abstract).
fn method_from_fun(
    f: skotch_ast::KtFun<'_>,
    method_idx: u32,
    is_abstract_default: bool,
    strings: &mut Vec<String>,
) -> MirFunction {
    method_from_fun_with_class(f, method_idx, is_abstract_default, strings, None, &[])
}

#[allow(clippy::too_many_arguments)]
fn method_from_fun_with_class(
    f: skotch_ast::KtFun<'_>,
    method_idx: u32,
    is_abstract_default: bool,
    strings: &mut Vec<String>,
    class_name: Option<&str>,
    field_names: &[(String, Ty)],
) -> MirFunction {
    let name = f.name().unwrap_or("<anon>").to_string();
    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let params: Vec<skotch_mir::LocalId> = (0..param_count)
        .map(|i| skotch_mir::LocalId(i as u32))
        .collect();
    let param_names: Vec<String> = f
        .value_parameter_list()
        .map(|pl| {
            pl.parameters()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    let return_ty = match f
        .return_type()
        .and_then(|tr| tr.user_type())
        .and_then(|u| u.name())
    {
        Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Any),
        None => Ty::Unit,
    };
    let has_body = f.body_block().is_some() || f.body_expression().is_some();
    let is_abstract = f.is_abstract() || (is_abstract_default && !has_body);
    // Try to lower a simple literal body. method_simple_body lays
    // out: local 0 = this; locals 1..N+1 = user params; local N+2 =
    // result. Bodies that can't be lowered fall back to an empty
    // Return placeholder.
    let (blocks, extra_locals) = if !is_abstract {
        method_simple_body_with_class(f, strings, class_name, field_names)
    } else {
        (
            vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            Vec::new(),
        )
    };
    // Local layout: this (Ty::Any placeholder), each user param (Ty::Any),
    // then any extra_locals from the body lowering.
    let mut locals: Vec<Ty> = Vec::with_capacity(1 + param_count + extra_locals.len());
    locals.push(Ty::Any); // this
    for _ in 0..param_count {
        locals.push(Ty::Any);
    }
    locals.extend(extra_locals);
    MirFunction {
        id: FuncId(method_idx),
        name,
        params,
        locals,
        blocks,
        return_ty,
        required_params: param_count,
        param_names,
        param_receiver_types: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract,
        vararg_index: None,
        exception_handlers: Vec::new(),
        is_suspend: f.is_suspend(),
        is_inline: f.is_inline(),
        has_type_params: f
            .type_parameter_list()
            .map(|tpl| tpl.parameters().next().is_some())
            .unwrap_or(false),
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
        named_locals: Vec::new(),
        is_private: f.visibility() == skotch_syntax::Visibility::Private,
        is_static: false,
        default_call_masks: Vec::new(),
        needs_leading_nop: false,
        local_generic_args: rustc_hash::FxHashMap::default(),
    }
}

/// Collect methods from a class body. Each becomes a MirFunction
/// with an empty Return body — body lowering is deferred.
fn collect_class_methods(
    c: skotch_ast::KtClass<'_>,
    class_name: &str,
    strings: &mut Vec<String>,
) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = c.body() else { return methods };
    // Collect (name, Ty) for primary-ctor val/var params — methods
    // can reference them as `this.x` (implicit) or bare `x`.
    let mut field_names: Vec<(String, Ty)> = Vec::new();
    if let Some(pc) = c.primary_constructor() {
        if let Some(plist) = pc.value_parameter_list() {
            for p in plist.parameters() {
                if p.is_val() || p.is_var() {
                    if let Some(n) = p.name() {
                        let ty = p
                            .type_reference()
                            .and_then(|tr| tr.user_type())
                            .and_then(|u| u.name())
                            .and_then(skotch_types::ty_from_name)
                            .unwrap_or(Ty::Any);
                        field_names.push((n.to_string(), ty));
                    }
                }
            }
        }
    }
    // Also include body properties.
    for d in body.declarations() {
        if let KtDecl::Property(p) = d {
            if let Some(n) = p.name() {
                let ty = p
                    .type_reference()
                    .and_then(|tr| tr.user_type())
                    .and_then(|u| u.name())
                    .and_then(skotch_types::ty_from_name)
                    .unwrap_or(Ty::Any);
                field_names.push((n.to_string(), ty));
            }
        }
    }
    for (method_idx, f) in body
        .declarations()
        .filter_map(|d| match d {
            KtDecl::Fun(fun) => Some(fun),
            _ => None,
        })
        .enumerate()
    {
        methods.push(method_from_fun_with_class(
            f,
            method_idx as u32,
            false,
            strings,
            Some(class_name),
            &field_names,
        ));
    }
    methods
}

/// Walk a `KtSuperTypeList` to extract (super_class, interfaces).
/// SUPER_TYPE_CALL_ENTRY counts as the super class; bare
/// SUPER_TYPE_ENTRY and DELEGATED_SUPER_TYPE_ENTRY count as
/// interfaces (in Kotlin, a class can only extend one other class
/// and the call entry is the construction).
fn collect_class_super_iface(
    list: Option<skotch_ast::KtSuperTypeList<'_>>,
) -> (Option<String>, Vec<String>) {
    let Some(l) = list else {
        return (None, Vec::new());
    };
    let mut super_class = None;
    let mut ifaces = Vec::new();
    for entry in l.entries() {
        let name = entry
            .type_reference()
            .and_then(|t| t.user_type())
            .and_then(|u| u.name())
            .map(|s| s.to_string());
        match (name, &entry) {
            (Some(n), skotch_ast::SuperTypeEntry::Call(_)) => super_class = Some(n),
            (Some(n), _) => ifaces.push(n),
            (None, _) => {}
        }
    }
    (super_class, ifaces)
}

/// Build a minimal `<init>()V` constructor for a class with no
/// declared primary or secondary ctors. Mirrors what kotlinc emits
/// for a class with no body (`class Foo`).
fn empty_constructor(class_name: &str) -> MirFunction {
    MirFunction {
        id: FuncId(0),
        name: "<init>".to_string(),
        params: Vec::new(),
        locals: vec![Ty::Class(class_name.to_string())],
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::Unit,
        required_params: 0,
        param_names: Vec::new(),
        param_receiver_types: Vec::new(),
        param_defaults: Vec::new(),
        is_abstract: false,
        vararg_index: None,
        exception_handlers: Vec::new(),
        is_suspend: false,
        is_inline: false,
        has_type_params: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
        named_locals: Vec::new(),
        is_private: false,
        is_static: false,
        default_call_masks: Vec::new(),
        needs_leading_nop: false,
        local_generic_args: rustc_hash::FxHashMap::default(),
    }
}

/// Lower a const initializer expression (val/property RHS) to a
/// `MirConst`. Only the simplest literal forms are recognized; more
/// complex initializers run inside <clinit> at runtime. Mirrors the
/// legacy `lower_const_init`.
fn lower_const_init_typed(e: skotch_ast::KtExpr<'_>) -> Option<skotch_mir::MirConst> {
    use skotch_ast::KtExpr;
    use skotch_mir::MirConst;
    match e {
        KtExpr::Boolean(_) => {
            // The boolean composite child is a KW_TRUE / KW_FALSE token.
            let is_true = skotch_ast::children(e.syntax())
                .iter()
                .any(|c| c.kind == skotch_syntax::SyntaxKind::KW_TRUE);
            Some(MirConst::Bool(is_true))
        }
        KtExpr::Integer(_) => {
            // Pull the integer literal text from the child INTEGER_LITERAL.
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if c.kind == skotch_syntax::SyntaxKind::INTEGER_LITERAL {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            })?;
            let v: i64 = text.parse().ok()?;
            // Mirror legacy: Int by default (cast).
            Some(MirConst::Int(v as i32))
        }
        KtExpr::Float(_) => {
            let text = skotch_ast::children(e.syntax()).iter().find_map(|c| {
                if matches!(
                    c.kind,
                    skotch_syntax::SyntaxKind::FLOAT_LITERAL
                        | skotch_syntax::SyntaxKind::DOUBLE_LITERAL
                ) {
                    if let skotch_sil::SilData::Token { text } = &c.data {
                        return Some(text.as_str());
                    }
                }
                None
            })?;
            let v: f64 = text.trim_end_matches(['f', 'F']).parse().ok()?;
            // Disambiguate Float vs Double from suffix.
            if text.ends_with('f') || text.ends_with('F') {
                Some(MirConst::Float(v as f32))
            } else {
                Some(MirConst::Double(v))
            }
        }
        KtExpr::Null(_) => Some(MirConst::Null),
        KtExpr::Parenthesized(p) => skotch_ast::children(p.syntax())
            .iter()
            .find_map(|c| KtExpr::cast(c).and_then(lower_const_init_typed)),
        // String templates require MirModule access to intern strings,
        // so defer until call sites can pass module in.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower(src: &str, wrapper: &str) -> MirModule {
        let parsed = skotch_ast::parse("test.kt", src);
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = skotch_typeck::typed::type_check(
            parsed.file(),
            &resolved,
            &mut interner,
            &mut diags,
            None,
        );
        lower_file(
            parsed.file(),
            &resolved,
            &typed,
            &mut interner,
            &mut diags,
            wrapper,
            None,
        )
    }

    #[test]
    fn typed_lower_file_scaffold_returns_wrapper() {
        let module = lower("fun main() {}", "TestKt");
        assert_eq!(module.wrapper_class, "TestKt");
    }

    #[test]
    fn typed_lower_fun_main_produces_mir_function() {
        let module = lower("fun main() {}", "TestKt");
        assert_eq!(module.functions.len(), 1);
        let f = &module.functions[0];
        assert_eq!(f.name, "main");
        assert_eq!(f.params.len(), 0);
        assert_eq!(f.return_ty, Ty::Unit);
        assert_eq!(f.blocks.len(), 1);
        assert!(matches!(f.blocks[0].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_fun_with_params_records_signature() {
        let module = lower("fun add(a: Int, b: Int): Int = 0", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.required_params, 2);
        assert_eq!(f.return_ty, Ty::Int);
        assert_eq!(f.param_names, vec!["a".to_string(), "b".to_string()]);
        // Two params (Int, Int) plus a result slot for the `= 0`
        // literal body lowering.
        assert_eq!(f.locals, vec![Ty::Int, Ty::Int, Ty::Int]);
    }

    #[test]
    fn typed_lower_multi_funs_get_sequential_ids() {
        let module = lower("fun a() {}\nfun b() {}\nfun c() {}", "TestKt");
        assert_eq!(module.functions.len(), 3);
        assert_eq!(module.functions[0].id.0, 0);
        assert_eq!(module.functions[1].id.0, 1);
        assert_eq!(module.functions[2].id.0, 2);
        assert_eq!(module.functions[0].name, "a");
        assert_eq!(module.functions[2].name, "c");
    }

    #[test]
    fn typed_lower_suspend_inline_flags_propagate() {
        let module = lower("suspend inline fun foo() {}", "TestKt");
        let f = &module.functions[0];
        assert!(f.is_suspend);
        assert!(f.is_inline);
    }

    #[test]
    fn typed_lower_private_fun_marked_private() {
        let module = lower("private fun secret() {}", "TestKt");
        let f = &module.functions[0];
        assert!(f.is_private);
    }

    #[test]
    fn typed_lower_const_val_emits_top_level_const() {
        let module = lower("const val MAX: Int = 42", "TestKt");
        assert_eq!(module.top_level_consts.len(), 1);
        let (name, _ty, c) = &module.top_level_consts[0];
        assert_eq!(name, "MAX");
        assert!(matches!(c, skotch_mir::MirConst::Int(42)));
    }

    #[test]
    fn typed_lower_top_val_emits_top_level_prop() {
        let module = lower("val HALF: Double = 0.5", "TestKt");
        assert_eq!(module.top_level_props.len(), 1);
        assert!(module.top_level_prop_names.contains("HALF"));
        let (name, _ty, c) = &module.top_level_props[0];
        assert_eq!(name, "HALF");
        assert!(matches!(c, skotch_mir::MirConst::Double(d) if (*d - 0.5).abs() < 1e-9));
    }

    #[test]
    fn typed_lower_empty_class_emits_mir_class() {
        let module = lower("class Foo", "TestKt");
        assert_eq!(module.classes.len(), 1);
        let c = &module.classes[0];
        assert_eq!(c.name, "Foo");
        assert!(!c.is_open);
        assert!(!c.is_abstract);
        assert!(!c.is_interface);
        assert!(c.super_class.is_none());
    }

    #[test]
    fn typed_lower_open_class_marks_open() {
        let module = lower("open class Foo", "TestKt");
        assert!(module.classes[0].is_open);
    }

    #[test]
    fn typed_lower_abstract_class_marks_abstract() {
        let module = lower("abstract class Foo", "TestKt");
        assert!(module.classes[0].is_abstract);
    }

    #[test]
    fn typed_lower_class_with_super_records_super_class() {
        let module = lower("open class Base\nclass Derived : Base()", "TestKt");
        let derived = module
            .classes
            .iter()
            .find(|c| c.name == "Derived")
            .expect("Derived");
        assert_eq!(derived.super_class.as_deref(), Some("Base"));
    }

    #[test]
    fn typed_lower_class_with_interface_records_interface() {
        let module = lower("interface I\nclass Foo : I", "TestKt");
        let foo = module
            .classes
            .iter()
            .find(|c| c.name == "Foo")
            .expect("Foo");
        assert_eq!(foo.interfaces, vec!["I".to_string()]);
    }

    #[test]
    fn typed_lower_sealed_class_is_open_and_abstract() {
        let module = lower("sealed class Tree", "TestKt");
        let c = &module.classes[0];
        assert!(c.is_open);
        assert!(c.is_abstract);
    }

    #[test]
    fn typed_lower_interface_marks_is_interface() {
        let module = lower("interface Printable", "TestKt");
        let c = &module.classes[0];
        assert!(c.is_interface);
        assert!(c.is_abstract);
    }

    #[test]
    fn typed_lower_object_singleton_marks_flag() {
        let module = lower("object Singleton", "TestKt");
        let c = &module.classes[0];
        assert!(c.is_object_singleton);
    }

    #[test]
    fn typed_lower_enum_class_marks_enum() {
        let module = lower("enum class Color { RED, GREEN, BLUE }", "TestKt");
        let c = &module.classes[0];
        assert_eq!(c.super_class.as_deref(), Some("java/lang/Enum"));
        assert!(module.enum_names.contains("Color"));
    }

    #[test]
    fn typed_lower_enum_class_entries_emitted_as_static_fields() {
        let module = lower("enum class Color { RED, GREEN, BLUE }", "TestKt");
        let c = &module.classes[0];
        let entry_names: Vec<&str> = c.static_fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(entry_names, vec!["RED", "GREEN", "BLUE"]);
        // Each entry's type is the enum class itself.
        for f in &c.static_fields {
            match &f.ty {
                Ty::Class(n) => assert_eq!(n, "Color"),
                other => panic!("expected Ty::Class(Color), got {other:?}"),
            }
        }
    }

    #[test]
    fn typed_lower_class_with_primary_ctor_emits_fields_and_ctor() {
        let module = lower("class Box(val x: Int, val y: Int)", "TestKt");
        let c = module.classes.iter().find(|c| c.name == "Box").unwrap();
        let field_names: Vec<&str> = c.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(field_names, vec!["x", "y"]);
        // Constructor signature.
        assert_eq!(c.constructor.required_params, 2);
        assert_eq!(
            c.constructor.param_names,
            vec!["x".to_string(), "y".to_string()]
        );
        assert_eq!(c.constructor.locals, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn typed_lower_interface_with_methods_marks_abstract() {
        let module = lower("interface Printable { fun pretty(): String }", "TestKt");
        let c = &module.classes[0];
        assert_eq!(c.methods.len(), 1);
        let m = &c.methods[0];
        assert_eq!(m.name, "pretty");
        // No body → abstract default kicks in.
        assert!(m.is_abstract);
    }

    #[test]
    fn typed_lower_expr_bodied_int_returns_int_literal() {
        let module = lower("fun answer(): Int = 42", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        assert_eq!(f.blocks.len(), 1);
        // First stmt: Assign(local 0, Const(Int(42)))
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0);
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
        assert_eq!(f.locals, vec![Ty::Int]);
    }

    #[test]
    fn typed_lower_expr_bodied_bool_returns_bool_literal() {
        let module = lower("fun ok(): Boolean = true", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Bool);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Bool(true))
                ));
            }
        }
    }

    #[test]
    fn typed_lower_expr_bodied_string_returns_string_const() {
        let module = lower("fun greet(): String = \"hi\"", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::String);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Const(skotch_mir::MirConst::String(sid)) => {
                    assert_eq!(module.strings[sid.0 as usize], "hi");
                }
                other => panic!("expected Const(String), got {other:?}"),
            },
        }
    }

    #[test]
    fn typed_lower_println_string_template_with_interp() {
        let module = lower(
            "fun greet(name: String) { println(\"Hello, $name\") }",
            "TestKt",
        );
        let f = &module.functions[0];
        // Last stmt should be Call(PrintlnConcat, [string_chunk, name]).
        let block = &f.blocks[0];
        match block.stmts.last().unwrap() {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::PrintlnConcat));
                    // 2 args: literal chunk + the $name reference.
                    assert_eq!(args.len(), 2);
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_println_int_literal() {
        let module = lower("fun main() { println(42) }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, .. } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                }
                _ => panic!("expected Call"),
            },
        }
        assert_eq!(f.locals, vec![Ty::Int, Ty::Unit]);
    }

    #[test]
    fn typed_lower_binary_param_plus_literal() {
        let module = lower("fun addOne(x: Int): Int = x + 1", "TestKt");
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // 2 stmts: Const(Int(1)) for the literal, BinOp for the add.
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 1); // literal goes to slot after param
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(1))
                ));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 2); // result slot
                match value {
                    skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                        assert!(matches!(op, skotch_mir::BinOp::AddI));
                        assert_eq!(lhs.0, 0); // param x
                        assert_eq!(rhs.0, 1); // literal 1
                    }
                    other => panic!("expected BinOp, got {other:?}"),
                }
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_when_expression_two_arms() {
        let module = lower(
            "fun name(x: Int): String = when (x) { 1 -> \"one\"; 2 -> \"two\"; else -> \"other\" }",
            "TestKt",
        );
        let f = &module.functions[0];
        // 6 blocks: cmp_1, then_1, cmp_2, then_2, else, join
        assert_eq!(f.blocks.len(), 6);
        // join block is index 5: ReturnValue.
        assert!(matches!(f.blocks[5].terminator, Terminator::ReturnValue(_)));
        // First cmp block branches into then_1 (index 1) or cmp_2 (index 2).
        assert!(matches!(
            f.blocks[0].terminator,
            Terminator::Branch {
                then_block: 1,
                else_block: 2,
                ..
            }
        ));
        // then blocks Goto(5) the join.
        assert!(matches!(f.blocks[1].terminator, Terminator::Goto(5)));
        assert!(matches!(f.blocks[3].terminator, Terminator::Goto(5)));
        // else block (index 4) also Goto(5).
        assert!(matches!(f.blocks[4].terminator, Terminator::Goto(5)));
    }

    #[test]
    fn typed_lower_throw_param() {
        let module = lower("fun fail(e: Throwable): Nothing = throw e", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        let block = &f.blocks[0];
        assert!(block.stmts.is_empty());
        match &block.terminator {
            Terminator::Throw(local) => assert_eq!(local.0, 0),
            other => panic!("expected Throw, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_if_expression_max_of_two() {
        let module = lower(
            "fun max(a: Int, b: Int): Int = if (a > b) a else b",
            "TestKt",
        );
        let f = &module.functions[0];
        // 4 blocks: cond, then, else, join.
        assert_eq!(f.blocks.len(), 4);
        // Block 0: cond computation + Branch.
        assert!(matches!(
            f.blocks[0].terminator,
            Terminator::Branch {
                then_block: 1,
                else_block: 2,
                ..
            }
        ));
        // Block 1: then arm, Goto(3).
        assert!(matches!(f.blocks[1].terminator, Terminator::Goto(3)));
        // Block 2: else arm, Goto(3).
        assert!(matches!(f.blocks[2].terminator, Terminator::Goto(3)));
        // Block 3: ReturnValue.
        assert!(matches!(f.blocks[3].terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_static_call_resolves_funcid() {
        let module = lower(
            "fun inner(): Int = 42\nfun outer(): Int = inner()",
            "TestKt",
        );
        let outer = &module.functions[1];
        assert_eq!(outer.name, "outer");
        match &outer.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Static(_)));
                    if let skotch_mir::CallKind::Static(callee_id) = kind {
                        assert_eq!(callee_id.0, 0); // inner is FuncId 0
                    }
                    assert!(args.is_empty());
                }
                _ => panic!("expected Call"),
            },
        }
        assert!(matches!(
            outer.blocks[0].terminator,
            Terminator::ReturnValue(_)
        ));
    }

    #[test]
    fn typed_lower_static_call_with_literal_args() {
        let module = lower(
            "fun add(a: Int, b: Int): Int = a + b\nfun main(): Int = add(1, 2)",
            "TestKt",
        );
        let main = &module.functions[1];
        // main's body: Const(1) → slot 0, Const(2) → slot 1,
        // Call(Static(add), [slot 0, slot 1]) → slot 2.
        assert_eq!(main.blocks[0].stmts.len(), 3);
        match &main.blocks[0].stmts[2] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Static(_)));
                    assert_eq!(args.len(), 2);
                    assert_eq!(args[0].0, 0);
                    assert_eq!(args[1].0, 1);
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_static_call_with_param_arg() {
        let module = lower(
            "fun double(x: Int): Int = x + x\nfun foo(n: Int): Int = double(n)",
            "TestKt",
        );
        let foo = &module.functions[1];
        // foo's body: Call(Static(double), [n_local])
        match &foo.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { args, .. } => {
                    assert_eq!(args.len(), 1);
                    assert_eq!(args[0].0, 0); // n is param 0
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_static_call_unit_return_uses_plain_return() {
        let module = lower("fun side() {}\nfun caller() = side()", "TestKt");
        let caller = &module.functions[1];
        // Unit-returning callee → Terminator is plain Return, not ReturnValue.
        assert!(matches!(caller.blocks[0].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_mixed_chained_binary() {
        // Tests that resolve_operand_rec handles nested Binary in
        // either operand position.
        let module = lower("fun f(a: Int): Int = (a + 1) * 2", "TestKt");
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // Stmts in order:
        //  Const(1) → slot 1
        //  BinOp(AddI, a=0, slot 1) → slot 2  (inner a + 1)
        //  Const(2) → slot 3
        //  BinOp(MulI, slot 2, slot 3) → slot 4 (outer)
        assert_eq!(block.stmts.len(), 4);
        match &block.stmts[3] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::MulI));
                }
                _ => panic!("expected BinOp"),
            },
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_as_cast_string() {
        let module = lower("fun toS(x: Any): String = x as String", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::String);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::CheckCast { obj, target_class } => {
                    assert_eq!(obj.0, 0);
                    assert!(
                        target_class == "java/lang/String"
                            || target_class == "String"
                    );
                }
                _ => panic!("expected CheckCast"),
            },
        }
    }

    #[test]
    fn typed_lower_fn_returns_top_level_val() {
        let module = lower(
            "val GREETING: String = \"hi\"\nfun greet(): String = GREETING",
            "TestKt",
        );
        let f = module.functions.iter().find(|f| f.name == "greet").unwrap();
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::GetStaticField {
                    class_name,
                    field_name,
                    descriptor,
                } => {
                    assert_eq!(class_name, "TestKt");
                    assert_eq!(field_name, "GREETING");
                    assert_eq!(descriptor, "Ljava/lang/String;");
                }
                _ => panic!("expected GetStaticField"),
            },
        }
    }

    #[test]
    fn typed_lower_is_expression_string_check() {
        let module = lower(
            "fun isStr(x: Any): Boolean = x is String",
            "TestKt",
        );
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Bool);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::InstanceOf { obj, type_descriptor } => {
                    assert_eq!(obj.0, 0); // x param
                    // String maps to java/lang/String.
                    assert!(
                        type_descriptor == "java/lang/String"
                            || type_descriptor == "String"
                    );
                }
                _ => panic!("expected InstanceOf"),
            },
        }
    }

    #[test]
    fn typed_lower_prefix_minus_param() {
        let module = lower("fun neg(x: Int): Int = -x", "TestKt");
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // Expected: Const(0) into slot 1, BinOp(SubI, slot 1, slot 0) into slot 2.
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(0))
                ));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                    assert!(matches!(op, skotch_mir::BinOp::SubI));
                    assert_eq!(lhs.0, 1); // 0
                    assert_eq!(rhs.0, 0); // x
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_chained_binary_add() {
        let module = lower(
            "fun sum3(a: Int, b: Int, c: Int): Int = a + b + c",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // 2 BinOp stmts: inner (a + b), outer (inner + c).
        assert_eq!(block.stmts.len(), 2);
        // First stmt: inner a + b
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddI));
                    assert_eq!(lhs.0, 0); // a
                    assert_eq!(rhs.0, 1); // b
                }
                _ => panic!("expected BinOp"),
            },
        }
        // Second stmt: outer Binary on inner_slot + c
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { rhs, .. } => {
                    assert_eq!(rhs.0, 2); // c
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_long_uses_addl() {
        let module = lower("fun add(a: Long, b: Long): Long = a + b", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Long);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddL));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_double_uses_addd() {
        let module = lower("fun add(a: Double, b: Double): Double = a + b", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Double);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddD));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_int_long_promotes_to_long() {
        // Mixed Int + Long should promote to Long.
        let module = lower("fun add(a: Int, b: Long): Long = a + b", "TestKt");
        let f = &module.functions[0];
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddL));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_identity_function() {
        let module = lower("fun id(x: Int): Int = x", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        // No intermediate slot — just ReturnValue on the param.
        assert!(block.stmts.is_empty());
        match &block.terminator {
            Terminator::ReturnValue(local) => assert_eq!(local.0, 0),
            other => panic!("expected ReturnValue, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_parenthesized_literal_body() {
        let module = lower("fun pi() = (42)", "TestKt");
        let f = &module.functions[0];
        // Same shape as fun pi() = 42 — parens are transparent.
        assert_eq!(f.return_ty, Ty::Int);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        assert!(matches!(f.blocks[0].terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_parenthesized_binary_body() {
        let module = lower("fun pi(x: Int, y: Int): Int = (x + y)", "TestKt");
        let f = &module.functions[0];
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddI));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_string_concat_with_param() {
        let module = lower(
            "fun greet(name: String): String = \"Hello, \" + name",
            "TestKt",
        );
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::String);
        match &f.blocks[0].stmts.last().unwrap() {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::ConcatStr));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_gt_comparison() {
        let module = lower("fun isPos(x: Int): Boolean = x > 0", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Bool);
        let block = &f.blocks[0];
        // 2 stmts: literal 0 then comparison
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::CmpGt));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_eq_returns_bool() {
        let module = lower("fun same(a: Int, b: Int): Boolean = a == b", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Bool);
        match &f.blocks[0].stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::CmpEq));
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_literal_minus_param() {
        let module = lower("fun negFrom(x: Int): Int = 0 - x", "TestKt");
        let f = &module.functions[0];
        let block = &f.blocks[0];
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(0))
                ));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                    assert!(matches!(op, skotch_mir::BinOp::SubI));
                    assert_eq!(lhs.0, 1); // literal 0
                    assert_eq!(rhs.0, 0); // param x
                }
                _ => panic!("expected BinOp"),
            },
        }
    }

    #[test]
    fn typed_lower_do_while_loop_with_println() {
        let module = lower(
            "fun loop(n: Int) { do { println(\"hi\") } while (n > 0) }",
            "TestKt",
        );
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 3);
        // Block 0: body (println), Goto(1).
        let body = &f.blocks[0];
        assert_eq!(body.stmts.len(), 2);
        assert!(matches!(body.terminator, Terminator::Goto(1)));
        // Block 1: cond, Branch(then=0, else=2).
        assert!(matches!(
            f.blocks[1].terminator,
            Terminator::Branch { then_block: 0, else_block: 2, .. }
        ));
        // Block 2: exit, Return.
        assert!(matches!(f.blocks[2].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_while_loop_with_println() {
        let module = lower(
            "fun loop(n: Int) { while (n > 0) { println(\"hi\") } }",
            "TestKt",
        );
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 3);
        // Body block has Assign(arg, Const("hi")) + Assign(_, Call(Println, _))
        let body = &f.blocks[1];
        assert_eq!(body.stmts.len(), 2);
        match &body.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, .. } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                }
                _ => panic!("expected Call"),
            },
        }
        assert!(matches!(body.terminator, Terminator::Goto(0)));
    }

    #[test]
    fn typed_lower_while_loop_empty_body() {
        let module = lower(
            "fun loop(n: Int) { while (n > 0) {} }",
            "TestKt",
        );
        let f = &module.functions[0];
        // 3 blocks: cond, body, exit.
        assert_eq!(f.blocks.len(), 3);
        // Block 0: cond computation + Branch.
        assert!(matches!(
            f.blocks[0].terminator,
            Terminator::Branch { then_block: 1, else_block: 2, .. }
        ));
        // Block 1: empty body, backward Goto(0).
        assert!(f.blocks[1].stmts.is_empty());
        assert!(matches!(f.blocks[1].terminator, Terminator::Goto(0)));
        // Block 2: exit, Return.
        assert!(matches!(f.blocks[2].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_local_val_then_println() {
        let module = lower("fun main() {\n  val x = 42\n  println(x)\n}", "TestKt");
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // 3 stmts: Assign val x, Assign println-result, ...
        // Actually 2 stmts: val x's Const, then Call(println, [x])
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0); // val x → local 0 (no params)
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                    // arg passes the local x.
                    assert_eq!(args[0].0, 0);
                }
                _ => panic!("expected Call"),
            },
        }
        assert!(matches!(block.terminator, Terminator::Return));
        // locals: x (Int), println-result (Unit)
        assert_eq!(f.locals, vec![Ty::Int, Ty::Unit]);
    }

    #[test]
    fn typed_lower_val_binary_init_then_println() {
        let module = lower(
            "fun show(a: Int, b: Int) {\n  val sum = a + b\n  println(sum)\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // 3 stmts: BinOp(sum), then Println call (no Const since arg is ref)
        // Actually since arg is a ref, no extra Const slot is needed.
        // stmts: [Assign(sum), Assign(result, Call(Println, [sum]))]
        assert_eq!(block.stmts.len(), 2);
        // First: val sum = a + b
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(value, skotch_mir::Rvalue::BinOp { .. }));
            }
        }
        // Second: println(sum) — arg is the sum local.
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                    assert_eq!(args[0].0, 2); // sum is at slot 2 (after params 0,1)
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_val_comparison_then_return_ref() {
        let module = lower(
            "fun isEq(a: Int, b: Int): Boolean {\n  val eq = a == b\n  return eq\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // val eq = a == b → BinOp(CmpEq) into slot 2 (after params 0,1).
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 2);
                match value {
                    skotch_mir::Rvalue::BinOp { op, .. } => {
                        assert!(matches!(op, skotch_mir::BinOp::CmpEq));
                    }
                    _ => panic!("expected BinOp"),
                }
            }
        }
        // Local type of eq is Bool.
        assert_eq!(f.locals[2], Ty::Bool);
        match &block.terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 2),
            other => panic!("expected ReturnValue, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_val_from_static_call_then_println() {
        let module = lower(
            "fun answer(): Int = 42\nfun main() {\n  val x = answer()\n  println(x)\n}",
            "TestKt",
        );
        let main = module.functions.iter().find(|m| m.name == "main").unwrap();
        let block = &main.blocks[0];
        // First stmt: Call(Static(answer)) into slot 0.
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0);
                match value {
                    skotch_mir::Rvalue::Call { kind, .. } => {
                        assert!(matches!(kind, skotch_mir::CallKind::Static(_)));
                    }
                    _ => panic!("expected Call"),
                }
            }
        }
        // Second stmt: Call(Println, [slot 0])
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                    assert_eq!(args[0].0, 0); // x's slot
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_chained_val_then_return() {
        let module = lower(
            "fun calc(a: Int, b: Int, c: Int): Int {\n  val ab = a + b\n  val sum = ab + c\n  return sum\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // Two BinOp stmts (ab and sum).
        assert_eq!(block.stmts.len(), 2);
        // First: ab = a + b → BinOp(0, 1) → slot 3.
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 3);
                match value {
                    skotch_mir::Rvalue::BinOp { lhs, rhs, .. } => {
                        assert_eq!(lhs.0, 0);
                        assert_eq!(rhs.0, 1);
                    }
                    _ => panic!("expected BinOp"),
                }
            }
        }
        // Second: sum = ab + c → BinOp(3, 2) → slot 4.
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 4);
                match value {
                    skotch_mir::Rvalue::BinOp { lhs, rhs, .. } => {
                        assert_eq!(lhs.0, 3); // ab
                        assert_eq!(rhs.0, 2); // c
                    }
                    _ => panic!("expected BinOp"),
                }
            }
        }
        // Return slot 4 (sum).
        match &block.terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 4),
            other => panic!("expected ReturnValue(4), got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_val_binary_init_then_return_ref() {
        let module = lower(
            "fun foo(a: Int, b: Int): Int {\n  val sum = a + b\n  return sum\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // val sum = a + b → BinOp(AddI, a=0, b=1) → slot 2.
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 2);
                match value {
                    skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                        assert!(matches!(op, skotch_mir::BinOp::AddI));
                        assert_eq!(lhs.0, 0);
                        assert_eq!(rhs.0, 1);
                    }
                    _ => panic!("expected BinOp"),
                }
            }
        }
        match &block.terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 2),
            other => panic!("expected ReturnValue(2), got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_val_then_return_ref() {
        let module = lower(
            "fun foo(): Int {\n  val x = 42\n  return x\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        let block = &f.blocks[0];
        // 1 stmt: Const(Int(42)) into local 0
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0);
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        // Terminator: ReturnValue(local 0) — the val's slot.
        match &block.terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 0),
            other => panic!("expected ReturnValue, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_two_vals_then_print() {
        let module = lower(
            "fun main() {\n  val a = \"hi\"\n  val b = a\n  print(b)\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        // Only `val b = a` is a non-literal init in body → not supported.
        // So this should fall back to empty Return placeholder.
        let block = &f.blocks[0];
        assert!(
            matches!(block.terminator, Terminator::Return),
            "expected fallback Return for val-from-ref init"
        );
    }

    #[test]
    fn typed_lower_print_string_literal() {
        let module = lower("fun main() { print(\"x\") }", "TestKt");
        let f = &module.functions[0];
        match &f.blocks[0].stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, .. } => {
                    // print() (no newline) gets the Print intrinsic.
                    assert!(matches!(kind, skotch_mir::CallKind::Print));
                }
                _ => panic!("expected Call"),
            },
        }
    }

    #[test]
    fn typed_lower_binary_add_of_params() {
        let module = lower("fun add(a: Int, b: Int): Int = a + b", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 2);
                match value {
                    skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                        assert!(matches!(op, skotch_mir::BinOp::AddI));
                        assert_eq!(lhs.0, 0); // param a
                        assert_eq!(rhs.0, 1); // param b
                    }
                    other => panic!("expected BinOp, got {other:?}"),
                }
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
        // locals: a, b, result
        assert_eq!(f.locals, vec![Ty::Int, Ty::Int, Ty::Int]);
    }

    #[test]
    fn typed_lower_println_string_literal() {
        let module = lower("fun main() { println(\"hello\") }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 2);
        // stmt 0: Assign local 0 = Const(String(sid))
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0);
                match value {
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::String(sid)) => {
                        assert_eq!(module.strings[sid.0 as usize], "hello");
                    }
                    other => panic!("expected Const(String), got {other:?}"),
                }
            }
        }
        // stmt 1: Assign local 1 = Call(Println, [local 0])
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 1);
                match value {
                    skotch_mir::Rvalue::Call { kind, args } => {
                        assert!(matches!(kind, skotch_mir::CallKind::Println));
                        assert_eq!(args.len(), 1);
                        assert_eq!(args[0].0, 0);
                    }
                    other => panic!("expected Call, got {other:?}"),
                }
            }
        }
        assert!(matches!(block.terminator, Terminator::Return));
        // Locals: 0 (String for arg), 1 (Unit for unused return)
        assert_eq!(f.locals, vec![Ty::String, Ty::Unit]);
    }

    #[test]
    fn typed_lower_block_bodied_fn_with_no_return_emits_empty_return() {
        // Block body with no return — still emits empty Return.
        let module = lower("fun main() { }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        assert!(f.blocks[0].stmts.is_empty());
        assert!(matches!(f.blocks[0].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_block_bodied_return_with_binary() {
        let module = lower("fun add(a: Int, b: Int): Int { return a + b }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::AddI));
                }
                _ => panic!("expected BinOp"),
            },
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_block_bodied_return_with_param_ref() {
        let module = lower("fun identity(x: Int): Int { return x }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        // Identity from return: empty stmts + ReturnValue(0)
        assert!(block.stmts.is_empty());
        match &block.terminator {
            Terminator::ReturnValue(local) => assert_eq!(local.0, 0),
            other => panic!("expected ReturnValue, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_block_bodied_fn_with_literal_return() {
        let module = lower("fun answer(): Int { return 7 }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(7))
                ));
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_nested_class_emits_outer_dollar_inner() {
        let module = lower("class Outer { class Inner }", "TestKt");
        assert!(module.classes.iter().any(|c| c.name == "Outer"));
        assert!(module.classes.iter().any(|c| c.name == "Outer$Inner"));
    }

    #[test]
    fn typed_lower_class_with_secondary_ctor_emits_extra_init() {
        let module = lower(
            "class Foo(val x: Int) { constructor(s: String) : this(s.length) {} }",
            "TestKt",
        );
        let foo = module
            .classes
            .iter()
            .find(|c| c.name == "Foo")
            .expect("Foo");
        assert_eq!(foo.secondary_constructors.len(), 1);
        let sc = &foo.secondary_constructors[0];
        assert_eq!(sc.name, "<init>");
        assert_eq!(sc.required_params, 1);
        assert_eq!(sc.param_names, vec!["s".to_string()]);
        assert_eq!(sc.locals, vec![Ty::String]);
    }

    #[test]
    fn typed_lower_class_with_companion_emits_sibling_class() {
        let module = lower(
            "class Foo { companion object { fun create(): Foo = TODO() } }",
            "TestKt",
        );
        let foo = module
            .classes
            .iter()
            .find(|c| c.name == "Foo")
            .expect("Foo");
        assert_eq!(foo.companion_class_name.as_deref(), Some("Foo$Companion"));
        let comp = module
            .classes
            .iter()
            .find(|c| c.name == "Foo$Companion")
            .expect("companion");
        assert!(comp.is_object_singleton);
        assert_eq!(comp.methods.len(), 1);
        assert_eq!(comp.methods[0].name, "create");
    }

    #[test]
    fn typed_lower_object_with_methods_emits_method_list() {
        let module = lower("object S { fun greet(): String = \"hi\" }", "TestKt");
        let c = &module.classes[0];
        assert!(c.is_object_singleton);
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name, "greet");
        assert_eq!(c.methods[0].return_ty, Ty::String);
    }

    #[test]
    fn typed_lower_class_method_returns_this() {
        let module = lower(
            "class Box { fun self(): Box = this }",
            "TestKt",
        );
        let box_class = module.classes.iter().find(|c| c.name == "Box").unwrap();
        let m = box_class.methods.iter().find(|m| m.name == "self").unwrap();
        let block = &m.blocks[0];
        assert!(block.stmts.is_empty());
        match &block.terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 0), // this
            other => panic!("expected ReturnValue(0), got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_class_method_virtual_call_sibling() {
        let module = lower(
            "class P { fun a(): Int = 1; fun b(): Int = a() }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").unwrap();
        let m = p.methods.iter().find(|m| m.name == "b").unwrap();
        let block = &m.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, args } => {
                    match kind {
                        skotch_mir::CallKind::Virtual { class_name, method_name } => {
                            assert_eq!(class_name, "P");
                            assert_eq!(method_name, "a");
                        }
                        _ => panic!("expected Virtual"),
                    }
                    assert_eq!(args.len(), 1);
                    assert_eq!(args[0].0, 0); // this
                }
                _ => panic!("expected Call"),
            },
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_class_method_throw_param() {
        let module = lower(
            "class P { fun fail(e: Throwable): Nothing = throw e }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").unwrap();
        let m = p.methods.iter().find(|m| m.name == "fail").unwrap();
        let block = &m.blocks[0];
        assert!(block.stmts.is_empty());
        match &block.terminator {
            Terminator::Throw(slot) => assert_eq!(slot.0, 1), // e is at slot 1 (past this)
            other => panic!("expected Throw, got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_class_method_println_literal() {
        let module = lower(
            "class P { fun show(): Unit = println(\"hi\") }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").unwrap();
        let m = p.methods.iter().find(|m| m.name == "show").unwrap();
        let block = &m.blocks[0];
        // 2 stmts: Const(arg) + Call(Println, [arg])
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::Call { kind, .. } => {
                    assert!(matches!(kind, skotch_mir::CallKind::Println));
                }
                _ => panic!("expected Call"),
            },
        }
        assert!(matches!(block.terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_class_method_binary_field_op_literal() {
        let module = lower(
            "class Box(val x: Int) { fun double(): Int = x * 2 }",
            "TestKt",
        );
        let box_class = module.classes.iter().find(|c| c.name == "Box").unwrap();
        let m = box_class.methods.iter().find(|m| m.name == "double").unwrap();
        let block = &m.blocks[0];
        // Expected stmts: GetField for x (slot 2), Const(2) (slot 3),
        // BinOp(MulI, slot 2, slot 3) (slot 4).
        assert_eq!(block.stmts.len(), 3);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(value, skotch_mir::Rvalue::GetField { .. }));
            }
        }
        match &block.stmts[1] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(2))
                ));
            }
        }
        match &block.stmts[2] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::BinOp { op, .. } => {
                    assert!(matches!(op, skotch_mir::BinOp::MulI));
                }
                _ => panic!("expected BinOp"),
            },
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_class_method_field_access() {
        let module = lower(
            "class Box(val x: Int) { fun get(): Int = x }",
            "TestKt",
        );
        let box_class = module.classes.iter().find(|c| c.name == "Box").unwrap();
        let get_m = box_class.methods.iter().find(|m| m.name == "get").unwrap();
        // locals: this, result
        assert_eq!(get_m.locals.len(), 2);
        let block = &get_m.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => match value {
                skotch_mir::Rvalue::GetField {
                    receiver,
                    class_name,
                    field_name,
                } => {
                    assert_eq!(receiver.0, 0); // this
                    assert_eq!(class_name, "Box");
                    assert_eq!(field_name, "x");
                }
                _ => panic!("expected GetField"),
            },
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_class_method_binary_op_of_params() {
        let module = lower(
            "class P { fun add(a: Int, b: Int): Int = a + b }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").unwrap();
        let m = p.methods.iter().find(|m| m.name == "add").unwrap();
        // locals: this (Any), a (Any), b (Any), result (Int)
        assert_eq!(m.locals, vec![Ty::Any, Ty::Any, Ty::Any, Ty::Int]);
        let block = &m.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 3);
                match value {
                    skotch_mir::Rvalue::BinOp { op, lhs, rhs } => {
                        assert!(matches!(op, skotch_mir::BinOp::AddI));
                        assert_eq!(lhs.0, 1); // a (1-indexed after this)
                        assert_eq!(rhs.0, 2); // b
                    }
                    _ => panic!("expected BinOp"),
                }
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_class_method_identity_param() {
        let module = lower(
            "class P { fun id(x: Int): Int = x }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").unwrap();
        let m = p.methods.iter().find(|m| m.name == "id").unwrap();
        assert!(m.blocks[0].stmts.is_empty());
        // Identity: ReturnValue on slot 1 (param x, after this at slot 0).
        match &m.blocks[0].terminator {
            Terminator::ReturnValue(slot) => assert_eq!(slot.0, 1),
            other => panic!("expected ReturnValue(1), got {other:?}"),
        }
    }

    #[test]
    fn typed_lower_class_method_with_literal_body() {
        // Class methods with literal expression bodies now get real
        // bodies (Assign + ReturnValue) via method_simple_body.
        let module = lower(
            "class P(val x: Int) { fun answer(): Int = 42 }",
            "TestKt",
        );
        let p = module.classes.iter().find(|c| c.name == "P").expect("P");
        let m = p.methods.iter().find(|m| m.name == "answer").expect("answer");
        // local 0: this, local 1: result (Int) — answer() has 0 params.
        assert_eq!(m.locals, vec![Ty::Any, Ty::Int]);
        assert_eq!(m.blocks.len(), 1);
        let block = &m.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 1); // slot after `this`
                assert!(matches!(
                    value,
                    skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))
                ));
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_interface_abstract_method_keeps_empty_body() {
        let module = lower(
            "interface I { fun pretty(): String }",
            "TestKt",
        );
        let i = module.classes.iter().find(|c| c.name == "I").expect("I");
        let m = &i.methods[0];
        assert!(m.is_abstract);
        // Abstract methods don't lower a body.
        assert!(m.blocks[0].stmts.is_empty());
        assert!(matches!(m.blocks[0].terminator, Terminator::Return));
    }

    #[test]
    fn typed_lower_object_method_with_string_literal() {
        let module = lower(
            "object S { fun greet(): String = \"hi\" }",
            "TestKt",
        );
        let s = module.classes.iter().find(|c| c.name == "S").expect("S");
        let m = &s.methods[0];
        assert_eq!(m.locals, vec![Ty::Any, Ty::String]);
        assert!(matches!(m.blocks[0].terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_class_with_methods_emits_method_signatures() {
        let module = lower(
            "class P(val x: Int) { fun double(): Int = 0; fun greet(): String = \"\" }",
            "TestKt",
        );
        let c = module.classes.iter().find(|c| c.name == "P").unwrap();
        let methods: Vec<(&str, &Ty)> = c
            .methods
            .iter()
            .map(|m| (m.name.as_str(), &m.return_ty))
            .collect();
        assert_eq!(methods, vec![("double", &Ty::Int), ("greet", &Ty::String)],);
    }

    #[test]
    fn typed_lower_top_val_with_no_literal_init() {
        // Non-literal init: const lowering returns None → MirConst::Null
        // placeholder; the real <clinit> path handles the actual init.
        let module = lower("val X = foo()", "TestKt");
        assert_eq!(module.top_level_props.len(), 1);
        assert!(matches!(
            module.top_level_props[0].2,
            skotch_mir::MirConst::Null
        ));
    }
}
