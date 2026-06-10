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
            let methods = collect_class_methods(c, &name);
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
            let companion_class_name = companion.map(|o| {
                let comp_simple = o.name().unwrap_or("Companion").to_string();
                let comp_qname = format!("{}${}", name, comp_simple);
                let comp_methods = collect_object_methods(o);
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
                // Need to defer pushing — we don't have `&mut module`
                // inside the closure. Build first, push after.
                (comp_qname, comp_class)
            });
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
                            let nested_methods = collect_class_methods(nested, &nested_qname);
                            let nested_ctor =
                                constructor_from_primary(nested, &nested_qname);
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
            let methods = collect_interface_methods(i);
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
            let methods = collect_object_methods(o);
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
            let (blocks, extra_locals) = lower_simple_body(f, &mut module.strings);

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

/// Try to lower a multi-statement block body using a simple local
/// tracking pass. Handles bodies whose statements are sequences of:
///   - val <name> = <literal>            (KtProperty)
///   - println(<ref-or-literal>)         (single-arg println call)
///   - print(<ref-or-literal>)           (single-arg print call)
///
/// Returns None for any unsupported statement.
fn try_lower_multi_stmt_block(
    block: skotch_ast::KtBlock<'_>,
    f: skotch_ast::KtFun<'_>,
    strings: &mut Vec<String>,
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

    for c in skotch_ast::children(block.syntax()) {
        if let Some(prop) = skotch_ast::KtProperty::cast(c) {
            // `val <name> = <literal>` — emit Assign + push local.
            let name = prop.name()?;
            let init = prop.initializer()?;
            let (k, ty) = literal_to_const(&init, strings)?;
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
        if let Some(expr) = KtExpr::cast(c) {
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
    let blocks = vec![BasicBlock {
        stmts,
        terminator: Terminator::Return,
    }];
    Some((blocks, local_tys))
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
            let stmts: Vec<KtExpr<'_>> = block.statements().collect();
            // Walk PROPERTY children + KtExpr stmts together via
            // try_lower_multi_stmt_block — this handles
            // `val x = 10; println(x)` and similar simple shapes.
            if let Some((blocks, locals)) =
                try_lower_multi_stmt_block(block, f, strings)
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
    // Binary arithmetic body where both operands are references to
    // the function's own parameters: `fun add(a: Int, b: Int) = a + b`.
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
        let lhs_idx = b.lhs().and_then(|l| match l {
            KtExpr::Reference(r) => r
                .name()
                .and_then(|n| param_names.iter().position(|p| p == n)),
            _ => None,
        });
        let rhs_idx = b.rhs().and_then(|r| match r {
            KtExpr::Reference(rr) => rr
                .name()
                .and_then(|n| param_names.iter().position(|p| p == n)),
            _ => None,
        });
        let op_text = b.operation().map(|o| o.text()).unwrap_or_default();
        // Typed param types determine which variant to use. With
        // current coverage we assume Int; long/float/double bodies
        // need explicit typed-Ty tracking which lands later.
        let mir_op = match op_text.as_str() {
            "+" => Some(skotch_mir::BinOp::AddI),
            "-" => Some(skotch_mir::BinOp::SubI),
            "*" => Some(skotch_mir::BinOp::MulI),
            "/" => Some(skotch_mir::BinOp::DivI),
            "%" => Some(skotch_mir::BinOp::ModI),
            _ => None,
        };
        if let (Some(lhs), Some(rhs), Some(op)) = (lhs_idx, rhs_idx, mir_op) {
            // Result type: pull from f.return_type.
            let return_ty = match f
                .return_type()
                .and_then(|tr| tr.user_type())
                .and_then(|u| u.name())
            {
                Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Int),
                None => Ty::Int,
            };
            let result_slot = skotch_mir::LocalId(param_count as u32);
            let blocks = vec![BasicBlock {
                stmts: vec![skotch_mir::Stmt::Assign {
                    dest: result_slot,
                    value: skotch_mir::Rvalue::BinOp {
                        op,
                        lhs: skotch_mir::LocalId(lhs as u32),
                        rhs: skotch_mir::LocalId(rhs as u32),
                    },
                }],
                terminator: Terminator::ReturnValue(result_slot),
            }];
            return (blocks, vec![return_ty]);
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
            let Some(text) = text else { return make_placeholder() };
            let Ok(v) = text.parse::<i64>() else { return make_placeholder() };
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
    let params: Vec<skotch_mir::LocalId> =
        (0..param_count).map(|i| skotch_mir::LocalId(i as u32)).collect();
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
                        let ty = match p.type_reference().and_then(|tr| tr.user_type()).and_then(|u| u.name()) {
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
                    let ty = match p.type_reference().and_then(|tr| tr.user_type()).and_then(|u| u.name()) {
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
        let params: Vec<skotch_mir::LocalId> =
            (0..param_count).map(|i| skotch_mir::LocalId(i as u32)).collect();
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
fn collect_interface_methods(i: skotch_ast::KtInterface<'_>) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = i.body() else { return methods };
    let mut method_idx = 0u32;
    for d in body.declarations() {
        if let KtDecl::Fun(f) = d {
            methods.push(method_from_fun(f, method_idx, true));
            method_idx += 1;
        }
    }
    methods
}

/// Same-shape helper for object singletons.
fn collect_object_methods(o: skotch_ast::KtObjectDeclaration<'_>) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = o.body() else { return methods };
    let mut method_idx = 0u32;
    for d in body.declarations() {
        if let KtDecl::Fun(f) = d {
            methods.push(method_from_fun(f, method_idx, false));
            method_idx += 1;
        }
    }
    methods
}

/// Build a MirFunction from a typed KtFun. `is_abstract_default`
/// applies when the source has no body and the surrounding decl is
/// an interface (where methods default abstract).
fn method_from_fun(
    f: skotch_ast::KtFun<'_>,
    method_idx: u32,
    is_abstract_default: bool,
) -> MirFunction {
    let name = f.name().unwrap_or("<anon>").to_string();
    let param_count = f
        .value_parameter_list()
        .map(|pl| pl.parameters().count())
        .unwrap_or(0);
    let params: Vec<skotch_mir::LocalId> =
        (0..param_count).map(|i| skotch_mir::LocalId(i as u32)).collect();
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
    let locals: Vec<Ty> = (0..param_count).map(|_| Ty::Any).collect();
    let has_body = f.body_block().is_some() || f.body_expression().is_some();
    let is_abstract = f.is_abstract() || (is_abstract_default && !has_body);
    MirFunction {
        id: FuncId(method_idx),
        name,
        params,
        locals,
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
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
fn collect_class_methods(c: skotch_ast::KtClass<'_>, _class_name: &str) -> Vec<MirFunction> {
    let mut methods = Vec::new();
    let Some(body) = c.body() else { return methods };
    let mut method_idx = 0u32;
    for d in body.declarations() {
        if let KtDecl::Fun(f) = d {
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
            // Return type: pulled from KtTypeReference (simple name only here).
            let return_ty = match f
                .return_type()
                .and_then(|tr| tr.user_type())
                .and_then(|u| u.name())
            {
                Some(name) => skotch_types::ty_from_name(name).unwrap_or(Ty::Any),
                None => Ty::Unit,
            };
            let locals: Vec<Ty> = (0..param_count).map(|_| Ty::Any).collect();
            methods.push(MirFunction {
                id: FuncId(method_idx),
                name,
                params,
                locals,
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty,
                required_params: param_count,
                param_names,
                param_receiver_types: Vec::new(),
                param_defaults: Vec::new(),
                is_abstract: f.is_abstract(),
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
            method_idx += 1;
        }
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
        assert_eq!(c.constructor.param_names, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(c.constructor.locals, vec![Ty::Int, Ty::Int]);
    }

    #[test]
    fn typed_lower_interface_with_methods_marks_abstract() {
        let module = lower(
            "interface Printable { fun pretty(): String }",
            "TestKt",
        );
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
                assert!(matches!(value, skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))));
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
                assert!(matches!(value, skotch_mir::Rvalue::Const(skotch_mir::MirConst::Bool(true))));
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
    fn typed_lower_println_int_literal() {
        let module = lower("fun main() { println(42) }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.blocks.len(), 1);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(value, skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))));
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
    fn typed_lower_local_val_then_println() {
        let module = lower(
            "fun main() {\n  val x = 42\n  println(x)\n}",
            "TestKt",
        );
        let f = &module.functions[0];
        let block = &f.blocks[0];
        // 3 stmts: Assign val x, Assign println-result, ...
        // Actually 2 stmts: val x's Const, then Call(println, [x])
        assert_eq!(block.stmts.len(), 2);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { dest, value } => {
                assert_eq!(dest.0, 0); // val x → local 0 (no params)
                assert!(matches!(value, skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(42))));
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
    fn typed_lower_block_bodied_fn_with_literal_return() {
        let module = lower("fun answer(): Int { return 7 }", "TestKt");
        let f = &module.functions[0];
        assert_eq!(f.return_ty, Ty::Int);
        let block = &f.blocks[0];
        assert_eq!(block.stmts.len(), 1);
        match &block.stmts[0] {
            skotch_mir::Stmt::Assign { value, .. } => {
                assert!(matches!(value, skotch_mir::Rvalue::Const(skotch_mir::MirConst::Int(7))));
            }
        }
        assert!(matches!(block.terminator, Terminator::ReturnValue(_)));
    }

    #[test]
    fn typed_lower_nested_class_emits_outer_dollar_inner() {
        let module = lower("class Outer { class Inner }", "TestKt");
        assert!(module.classes.iter().any(|c| c.name == "Outer"));
        assert!(module
            .classes
            .iter()
            .any(|c| c.name == "Outer$Inner"));
    }

    #[test]
    fn typed_lower_class_with_secondary_ctor_emits_extra_init() {
        let module = lower(
            "class Foo(val x: Int) { constructor(s: String) : this(s.length) {} }",
            "TestKt",
        );
        let foo = module.classes.iter().find(|c| c.name == "Foo").expect("Foo");
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
        let foo = module.classes.iter().find(|c| c.name == "Foo").expect("Foo");
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
        let module = lower(
            "object S { fun greet(): String = \"hi\" }",
            "TestKt",
        );
        let c = &module.classes[0];
        assert!(c.is_object_singleton);
        assert_eq!(c.methods.len(), 1);
        assert_eq!(c.methods[0].name, "greet");
        assert_eq!(c.methods[0].return_ty, Ty::String);
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
        assert_eq!(
            methods,
            vec![("double", &Ty::Int), ("greet", &Ty::String)],
        );
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
