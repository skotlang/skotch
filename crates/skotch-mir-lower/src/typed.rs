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
            // Single empty basic block terminating in Return.
            // This is the minimum viable body — further statement
            // lowering lands in follow-up porting steps. Even a
            // non-Unit-returning fn body produces a Return terminator
            // here as the placeholder; a future port emits
            // ReturnValue with the lowered last expression.
            let blocks = vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }];

            module.functions.push(MirFunction {
                id: FuncId(fn_id),
                name,
                params,
                locals: param_tys,
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
    let mut sc_idx = 0u32;
    for sc in body.secondary_constructors() {
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
        sc_idx += 1;
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
        assert_eq!(f.locals, vec![Ty::Int, Ty::Int]);
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
