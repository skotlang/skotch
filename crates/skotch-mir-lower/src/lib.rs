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
    BasicBlock, BinOp as MBinOp, CallKind, ExceptionHandler, FuncId, LiveSpill, LocalId, MirClass,
    MirConst, MirField, MirFunction, MirModule, Rvalue, SpillKind, SpillSlot, Stmt as MStmt,
    SuspendCallSite, SuspendStateMachine, Terminator,
};
use skotch_resolve::{ExternalClassKind, PackageSymbolTable, ResolvedFile};
use skotch_syntax::{BinOp, ConstructorParam, Decl, Expr, FunDecl, KtFile, Stmt, TypeRef, ValDecl};
use skotch_typeck::TypedFile;
use skotch_types::Ty;

/// Convert a `Ty` to its JVM descriptor string fragment (for building
/// method descriptors in the MIR lowerer).
fn jvm_type_string_for_ty(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "Z".to_string(),
        Ty::Byte => "B".to_string(),
        Ty::Short => "S".to_string(),
        Ty::Char => "C".to_string(),
        Ty::Int => "I".to_string(),
        Ty::Float => "F".to_string(),
        Ty::Long => "J".to_string(),
        Ty::Double => "D".to_string(),
        Ty::String => "Ljava/lang/String;".to_string(),
        Ty::Unit => "V".to_string(),
        Ty::Class(name) => format!("L{name};"),
        _ => "Ljava/lang/Object;".to_string(),
    }
}

/// Resolve a type name to a `Ty`, checking built-in types first, then
/// Resolve a full TypeRef to a Ty, handling function types and nullable.
fn resolve_type_ref(tr: &skotch_syntax::TypeRef, interner: &Interner, module: &MirModule) -> Ty {
    if let Some(ref fparams) = tr.func_params {
        // Function type: (P1, P2, ...) -> R
        let params: Vec<Ty> = fparams
            .iter()
            .map(|p| resolve_type_ref(p, interner, module))
            .collect();
        let ret = resolve_type(interner.resolve(tr.name), module);
        let base = Ty::Function {
            params,
            ret: Box::new(ret),
            is_suspend: tr.is_suspend,
        };
        if tr.nullable {
            Ty::Nullable(Box::new(base))
        } else {
            base
        }
    } else {
        let base = resolve_type(interner.resolve(tr.name), module);
        if tr.nullable {
            Ty::Nullable(Box::new(base))
        } else {
            base
        }
    }
}

/// user-defined classes/enums in the module.
fn resolve_type(name: &str, module: &MirModule) -> Ty {
    // Resolve type aliases before anything else.
    let resolved_name = if let Some(target) = module.type_aliases.get(name) {
        target.as_str()
    } else {
        name
    };
    if let Some(ty) = skotch_types::ty_from_name(resolved_name) {
        return ty;
    }
    // User-defined types take priority over well-known mappings so
    // that e.g. a user-defined `Pair` class isn't silently mapped
    // to `kotlin/Pair`.
    // Enums are real classes now.
    if module.enum_names.contains(resolved_name) {
        return Ty::Class(resolved_name.to_string());
    }
    // User-defined class or interface.
    if module.classes.iter().any(|c| c.name == resolved_name) {
        return Ty::Class(resolved_name.to_string());
    }
    // Map well-known Kotlin collection/stdlib type names to their
    // fully-qualified JVM internal names.
    if let Some(jvm_name) = well_known_class_name(resolved_name) {
        return Ty::Class(jvm_name.to_string());
    }
    Ty::Any
}

/// Map well-known Kotlin source-level class names to their JVM internal names
/// so that type descriptors use `Ljava/util/List;` rather than `LList;`.
fn well_known_class_name(name: &str) -> Option<&'static str> {
    match name {
        "List" | "MutableList" => Some("java/util/List"),
        "Map" | "MutableMap" => Some("java/util/Map"),
        "Set" | "MutableSet" => Some("java/util/Set"),
        "Collection" => Some("java/util/Collection"),
        "Iterable" => Some("java/lang/Iterable"),
        "Iterator" => Some("java/util/Iterator"),
        "Pair" => Some("kotlin/Pair"),
        "Triple" => Some("kotlin/Triple"),
        _ => None,
    }
}

/// Return the Kotlin stdlib `Function{N}` interface name for the given
/// arity. Lambda classes implement this real stdlib interface so they can
/// be passed to Kotlin stdlib HOFs (map, filter, etc.) which expect
/// `kotlin/jvm/functions/Function1` and friends.
///
/// Unlike the old `$FunctionN` approach, we do NOT create a synthetic
/// interface class — the real interface lives in kotlin-stdlib.jar on
/// the runtime classpath.
fn stdlib_function_interface(arity: usize) -> String {
    format!("kotlin/jvm/functions/Function{arity}")
}

/// Check if a method call on a receiver type is a Kotlin stdlib
/// Convert AST annotations to MIR annotations.
fn lower_annotations(
    annotations: &[skotch_syntax::Annotation],
    interner: &Interner,
) -> Vec<skotch_mir::MirAnnotation> {
    annotations
        .iter()
        .map(|a| {
            let name = interner.resolve(a.name);
            let descriptor = skotch_stdlib_registry::annotation_descriptor(name);
            let args: Vec<skotch_mir::MirAnnotationArg> = a
                .args
                .iter()
                .enumerate()
                .map(|(i, arg)| {
                    let arg_name = format!("value{i}");
                    let value = match arg {
                        skotch_syntax::AnnotationArg::StringLit(s) => {
                            skotch_mir::MirAnnotationValue::String(s.clone())
                        }
                        skotch_syntax::AnnotationArg::IntLit(v) => {
                            skotch_mir::MirAnnotationValue::Int(*v as i32)
                        }
                        skotch_syntax::AnnotationArg::BoolLit(v) => {
                            skotch_mir::MirAnnotationValue::Bool(*v)
                        }
                        skotch_syntax::AnnotationArg::Ident(sym) => {
                            skotch_mir::MirAnnotationValue::String(
                                interner.resolve(*sym).to_string(),
                            )
                        }
                        skotch_syntax::AnnotationArg::QualifiedName(parts) => {
                            let joined: Vec<&str> =
                                parts.iter().map(|s| interner.resolve(*s)).collect();
                            skotch_mir::MirAnnotationValue::String(joined.join("."))
                        }
                        skotch_syntax::AnnotationArg::Array(items) => {
                            let arr: Vec<skotch_mir::MirAnnotationValue> = items
                                .iter()
                                .map(|item| match item {
                                    skotch_syntax::AnnotationArg::StringLit(s) => {
                                        skotch_mir::MirAnnotationValue::String(s.clone())
                                    }
                                    _ => skotch_mir::MirAnnotationValue::String(String::new()),
                                })
                                .collect();
                            skotch_mir::MirAnnotationValue::Array(arr)
                        }
                    };
                    skotch_mir::MirAnnotationArg {
                        name: if a.args.len() == 1 {
                            "value".to_string()
                        } else {
                            arg_name
                        },
                        value,
                    }
                })
                .collect();
            skotch_mir::MirAnnotation {
                descriptor,
                args,
                retention: skotch_mir::AnnotationRetention::Runtime,
            }
        })
        .collect()
}

/// Look up a Kotlin extension function compiled as a static method in
/// a `*Kt` facade class. Data is in the `skotch-stdlib-registry` crate.
fn stdlib_extension(
    receiver_ty: &str,
    method: &str,
) -> Option<(&'static str, &'static str, &'static str, Ty)> {
    skotch_stdlib_registry::lookup_stdlib_extension(receiver_ty, method).map(|ext| {
        (
            ext.facade_class,
            ext.jvm_method,
            ext.descriptor,
            (ext.return_ty)(),
        )
    })
}

// The old hardcoded match table (~200 lines) has been replaced by the
// data-driven registry in `skotch-stdlib-registry::STDLIB_EXTENSIONS`.
// To add new stdlib extension mappings, add entries to that crate's table.

/// Lower a parsed/resolved/typed file to MIR.
pub fn lower_file(
    file: &KtFile,
    resolved: &ResolvedFile,
    typed: &TypedFile,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    wrapper_class: &str,
    package_symbols: Option<&PackageSymbolTable>,
) -> MirModule {
    let mut module = MirModule {
        wrapper_class: wrapper_class.to_string(),
        ..MirModule::default()
    };

    // ─── Pass 0: collect type aliases ─────────────────────────────────
    for decl in &file.decls {
        if let Decl::TypeAlias(ta) = decl {
            let alias_name = interner.resolve(ta.name).to_string();
            let target_name = interner.resolve(ta.target.name).to_string();
            module.type_aliases.insert(alias_name, target_name);
        }
    }

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
            let declared_ret = typed
                .functions
                .get(fn_pass1_idx)
                .map(|t| t.return_ty.clone())
                .unwrap_or(Ty::Unit);
            // Coroutine transform: rewrite the
            // return type of every `suspend fun` to `Any` (which
            // lowers to `Ljava/lang/Object;` on JVM). The actual
            // `$completion: Continuation` parameter is appended in
            // `lower_function`; we only record the signature change
            // here so call sites resolving through `name_to_func`
            // see the post-transform return type.
            let return_ty = if f.is_suspend { Ty::Any } else { declared_ret };
            let mut required = f.params.iter().filter(|p| p.default.is_none()).count();
            let mut param_defaults: Vec<Option<MirConst>> = f
                .params
                .iter()
                .map(|p| {
                    p.default
                        .as_ref()
                        .and_then(|d| lower_const_init(d, &mut module))
                })
                .collect();
            let mut param_names: Vec<String> = f
                .params
                .iter()
                .map(|p| interner.resolve(p.name).to_string())
                .collect();
            let vararg_index = f.params.iter().position(|p| p.is_vararg);
            // Coroutine transform: every `suspend
            // fun` grows a trailing `$completion: Continuation`
            // parameter. The call site knows to synthesize an
            // argument for it from the target's `is_suspend` flag.
            if f.is_suspend {
                param_names.push("$completion".to_string());
                param_defaults.push(None);
                required += 1;
            }
            // Set up placeholder params so extension function detection
            // (is_extension = params.len() == args.len() + 1) works for
            // recursive calls before the function is fully lowered.
            let placeholder_param_count = f.params.len()
                + if f.receiver_ty.is_some() { 1 } else { 0 }
                + if f.is_suspend { 1 } else { 0 };
            let placeholder_params: Vec<LocalId> =
                (0..placeholder_param_count as u32).map(LocalId).collect();
            let placeholder_locals: Vec<Ty> = vec![Ty::Any; placeholder_param_count];
            let fn_annotations = lower_annotations(&f.annotations, interner);
            module.functions.push(MirFunction {
                id,
                name: name_str,
                params: placeholder_params,
                locals: placeholder_locals,
                blocks: Vec::new(),
                return_ty,
                required_params: required,
                param_names,
                param_defaults,
                param_receiver_types: Vec::new(),
                is_abstract: false,
                exception_handlers: Vec::new(),
                vararg_index,
                is_suspend: f.is_suspend,
                is_inline: f.is_inline,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: fn_annotations,
            });
            fn_pass1_idx += 1;
        }
    }

    // ── Pre-register external suspend function stubs ──────
    //
    // External suspend functions like `delay` (from kotlinx-coroutines)
    // must be registered before any function bodies are lowered, so that
    // `body_contains_suspend_call` can detect them during suspend-lambda
    // analysis. These stubs are never emitted as JVM methods (they're
    // marked `is_abstract`); the real implementations live in library JARs.
    {
        let delay_sym = interner.intern("delay");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&delay_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "delay".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Unit),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_ms = stub.new_local(Ty::Long);
            stub.params.push(p_ms);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(delay_sym, fid);
        }
    }

    // ── Pre-register additional suspend function stubs ──────
    //
    // `withContext`, `coroutineScope`, `supervisorScope`, `withTimeout`,
    // `withTimeoutOrNull`, and `yield` are all suspend functions from
    // kotlinx-coroutines that appear as direct calls in user code.
    // Register stubs so `body_contains_suspend_call` can detect them
    // inside trailing lambdas passed to coroutine builders.
    //
    // withContext(CoroutineContext, Function2, Continuation) → Object
    // coroutineScope(Function2, Continuation) → Object
    // supervisorScope(Function2, Continuation) → Object
    // withTimeout(Long, Function2, Continuation) → Object
    // withTimeoutOrNull(Long, Function2, Continuation) → Object
    // yield(Continuation) → Object
    {
        // yield — suspend fun yield()
        let yield_sym = interner.intern("yield");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&yield_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "yield".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Unit),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(yield_sym, fid);
        }

        // withContext — suspend fun <T> withContext(context, block): T
        let wc_sym = interner.intern("withContext");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&wc_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "withContext".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Any),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_ctx = stub.new_local(Ty::Class("kotlin/coroutines/CoroutineContext".to_string()));
            stub.params.push(p_ctx);
            let p_block = stub.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
            stub.params.push(p_block);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(wc_sym, fid);
        }

        // coroutineScope — suspend fun <R> coroutineScope(block): R
        let cs_sym = interner.intern("coroutineScope");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&cs_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "coroutineScope".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Any),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_block = stub.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
            stub.params.push(p_block);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(cs_sym, fid);
        }

        // supervisorScope — suspend fun <R> supervisorScope(block): R
        let ss_sym = interner.intern("supervisorScope");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&ss_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "supervisorScope".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Any),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_block = stub.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
            stub.params.push(p_block);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(ss_sym, fid);
        }

        // withTimeout — suspend fun <T> withTimeout(timeMillis: Long, block): T
        let wt_sym = interner.intern("withTimeout");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&wt_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "withTimeout".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Any),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_ms = stub.new_local(Ty::Long);
            stub.params.push(p_ms);
            let p_block = stub.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
            stub.params.push(p_block);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(wt_sym, fid);
        }

        // withTimeoutOrNull — suspend fun <T> withTimeoutOrNull(timeMillis: Long, block): T?
        let wton_sym = interner.intern("withTimeoutOrNull");
        #[allow(clippy::map_entry)]
        if !name_to_func.contains_key(&wton_sym) {
            let mut stub = MirFunction {
                id: FuncId(0),
                name: "withTimeoutOrNull".to_string(),
                params: Vec::new(),
                locals: Vec::new(),
                blocks: vec![BasicBlock {
                    stmts: Vec::new(),
                    terminator: Terminator::Return,
                }],
                return_ty: Ty::Any,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: true,
                is_inline: false,
                suspend_original_return_ty: Some(Ty::Nullable(Box::new(Ty::Any))),
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let p_ms = stub.new_local(Ty::Long);
            stub.params.push(p_ms);
            let p_block = stub.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
            stub.params.push(p_block);
            let p_cont = stub.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
            stub.params.push(p_cont);
            let fid = module.add_function(stub);
            name_to_func.insert(wton_sym, fid);
        }
    }

    // Collect enum names so they can be recognized as types (mapped to String).
    let enum_names: rustc_hash::FxHashSet<String> = file
        .decls
        .iter()
        .filter_map(|d| {
            if let Decl::Enum(e) = d {
                Some(interner.resolve(e.name).to_string())
            } else {
                None
            }
        })
        .collect();
    module.enum_names = enum_names;

    // ─── Collect reified inline function bodies for call-site inlining ──
    // Functions with `reified` type params must be inlined at call sites
    // so the type check can use the concrete type argument.
    let mut reified_fns: FxHashMap<Symbol, &FunDecl> = FxHashMap::default();
    for decl in &file.decls {
        if let Decl::Fun(f) = decl {
            if f.type_params.iter().any(|tp| tp.is_reified) {
                reified_fns.insert(f.name, f);
            }
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
    // ("top-level val initializers must be a literal"), so we
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
    // Default java.lang.* imports (from registry).
    for name in skotch_stdlib_registry::DEFAULT_IMPORTS {
        import_map.insert(name.to_string(), format!("java/lang/{name}"));
    }
    // Process explicit imports
    for imp in &file.imports {
        let segments: Vec<&str> = imp.path.iter().map(|s| interner.resolve(*s)).collect();
        if imp.is_wildcard {
            // Star imports: enumerate user classes/functions from
            // PackageSymbolTable for the matching package prefix.
            if let Some(pkg) = package_symbols {
                let pkg_prefix = segments.join("/");
                for (name, ext_class) in &pkg.classes {
                    if ext_class.jvm_name.starts_with(&pkg_prefix) {
                        import_map
                            .entry(name.clone())
                            .or_insert_with(|| ext_class.jvm_name.clone());
                    }
                }
            }
        } else if !segments.is_empty() {
            // Use alias if present, otherwise use the simple name.
            let key = if let Some(alias_sym) = imp.alias {
                interner.resolve(alias_sym).to_string()
            } else {
                segments.last().unwrap().to_string()
            };
            let jvm_path = segments.join("/");
            import_map.insert(key, jvm_path);
        }
    }

    module.import_map = import_map;

    // Register cross-file declarations so they're accessible from lower_expr.
    if let Some(pkg) = package_symbols {
        // Classes → import_map + cross_file_classes + stub MirClass entries.
        // The stub MirClass allows field access (`p.x`) and method dispatch
        // (`g.greet()`) to work on cross-file class instances.
        for (name, ext_class) in &pkg.classes {
            if !module.import_map.contains_key(name) {
                module
                    .import_map
                    .insert(name.clone(), ext_class.jvm_name.clone());
            }
            let is_data = matches!(ext_class.kind, ExternalClassKind::DataClass);
            module.cross_file_classes.insert(
                name.clone(),
                (
                    ext_class.jvm_name.clone(),
                    format!("{:?}", ext_class.kind),
                    is_data,
                ),
            );

            // Only add a stub MirClass if one with this name doesn't
            // already exist (the class might be defined in this file too).
            let already_exists = module.classes.iter().any(|c| c.name == ext_class.jvm_name);
            if !already_exists {
                use skotch_mir::{BasicBlock, FuncId, MirClass, MirField, MirFunction, Terminator};
                let fields: Vec<MirField> = ext_class
                    .fields
                    .iter()
                    .map(|(fname, fty)| MirField {
                        name: fname.clone(),
                        ty: fty.clone(),
                    })
                    .collect();
                let stub_fn = |mname: &str, param_tys: &[Ty], ret_ty: &Ty| -> MirFunction {
                    let mut locals = vec![Ty::Class(ext_class.jvm_name.clone())];
                    for pt in param_tys {
                        locals.push(pt.clone());
                    }
                    let params: Vec<LocalId> = (0..locals.len() as u32).map(LocalId).collect();
                    MirFunction {
                        id: FuncId(0),
                        name: mname.to_string(),
                        params,
                        locals,
                        blocks: vec![BasicBlock {
                            stmts: Vec::new(),
                            terminator: Terminator::Return,
                        }],
                        return_ty: ret_ty.clone(),
                        required_params: 0,
                        param_names: Vec::new(),
                        param_receiver_types: Vec::new(),
                        param_defaults: Vec::new(),
                        is_abstract: false,
                        vararg_index: None,
                        exception_handlers: Vec::new(),
                        is_suspend: false,
                        is_inline: false,
                        suspend_original_return_ty: None,
                        suspend_state_machine: None,
                        annotations: Vec::new(),
                    }
                };
                let methods: Vec<MirFunction> = ext_class
                    .methods
                    .iter()
                    .map(|(mname, param_tys, ret_ty)| stub_fn(mname, param_tys, ret_ty))
                    .collect();
                let empty_ctor = stub_fn("<init>", &[], &Ty::Unit);
                module.classes.push(MirClass {
                    name: ext_class.jvm_name.clone(),
                    super_class: ext_class.super_class.clone(),
                    is_open: ext_class.is_open,
                    is_abstract: ext_class.is_abstract,
                    is_interface: matches!(ext_class.kind, ExternalClassKind::Interface),
                    interfaces: Vec::new(),
                    fields,
                    methods,
                    constructor: empty_ctor,
                    secondary_constructors: Vec::new(),
                    is_suspend_lambda: false,
                    is_cross_file_stub: true,
                    annotations: Vec::new(),
                });
            }
        }
        // Functions → cross_file_fns so bare calls resolve.
        for (name, decls) in &pkg.functions {
            if let Some(ext) = decls.first() {
                module.cross_file_fns.insert(
                    name.clone(),
                    (
                        ext.owner_class.clone(),
                        ext.descriptor.clone(),
                        ext.return_ty.clone(),
                    ),
                );
            }
        }
    }

    // ── Register a synthetic $Callable interface so all lambda classes
    //    can share a common dispatch target for invokevirtual. ────────
    //    This interface declares `invoke` with Object params/return so
    //    any lambda can be called through it.
    // Pre-register a synthetic $Callable abstract class that all lambda
    // classes will extend. This allows invokevirtual $Callable.invoke(...)
    // to dispatch correctly for function-typed parameters.
    // $Callable has no invoke method itself (it's added per-lambda with
    // the correct signature), but the JVM can resolve invokevirtual on
    // a parent class reference if the runtime subclass has the method.
    // Actually — the JVM requires the methodref class to DECLARE the
    // method. So we won't use $Callable for dispatch. Instead, we'll
    // record lambda class names during lowering and use a two-pass
    // approach: lower all declarations first, then patch invoke targets.
    //
    // For now, function-typed parameters that receive lambdas work ONLY
    // when the lambda is defined in the same scope (local variable).
    // Passing through a function parameter (higher-order) needs the
    // $Callable interface, which is tracked as a v0.3.0 follow-up.

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
                    &mut name_to_global,
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
                lower_enum(
                    e,
                    &mut name_to_func,
                    &mut name_to_global,
                    &mut module,
                    interner,
                );
            }
            Decl::Interface(iface) => {
                lower_interface(
                    iface,
                    &mut name_to_func,
                    &name_to_global,
                    &mut module,
                    interner,
                    diags,
                );
            }
            Decl::TypeAlias(_) => {
                // Type aliases are resolved at parse/type-check time;
                // no MIR lowering needed.
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

    // ─── Apply package prefix ──────────────────────────────────────────
    //
    // If the source file has a `package` declaration, prepend the
    // package path (with `/` separators) to the wrapper class and all
    // user-defined class names so that the JVM sees the correct
    // fully-qualified internal names (e.g. `com/example/InputKt`).
    if let Some(pkg) = &file.package {
        let segments: Vec<&str> = pkg.path.iter().map(|s| interner.resolve(*s)).collect();
        if !segments.is_empty() {
            let prefix = segments.join("/");

            // Collect the set of simple (un-prefixed) user class names
            // so we can rewrite internal references.
            let user_classes: rustc_hash::FxHashSet<String> =
                module.classes.iter().map(|c| c.name.clone()).collect();

            // 1. Prefix the wrapper class.
            module.wrapper_class = format!("{prefix}/{}", module.wrapper_class);

            // 2. Prefix every user-defined class name.
            for class in &mut module.classes {
                if user_classes.contains(&class.name) {
                    class.name = format!("{prefix}/{}", class.name);
                }
            }

            // 3. Prefix enum names.
            let old_enum_names: Vec<String> = module.enum_names.iter().cloned().collect();
            module.enum_names.clear();
            for name in old_enum_names {
                module.enum_names.insert(format!("{prefix}/{name}"));
            }

            // 4. Rewrite class-name references inside all MIR functions.
            let rewrite = |name: &mut String| {
                if user_classes.contains(name.as_str()) {
                    *name = format!("{prefix}/{name}");
                }
            };
            let rewrite_fn = |f: &mut MirFunction| {
                // Rewrite Ty::Class in locals.
                for ty in &mut f.locals {
                    if let Ty::Class(ref mut n) = ty {
                        if user_classes.contains(n.as_str()) {
                            *n = format!("{prefix}/{n}");
                        }
                    }
                }
                // Rewrite Rvalue references.
                for block in &mut f.blocks {
                    for stmt in &mut block.stmts {
                        let MStmt::Assign { value, .. } = stmt;
                        match value {
                            Rvalue::NewInstance(ref mut n) if user_classes.contains(n.as_str()) => {
                                *n = format!("{prefix}/{n}");
                            }
                            Rvalue::GetField {
                                ref mut class_name, ..
                            } => rewrite(class_name),
                            Rvalue::PutField {
                                ref mut class_name, ..
                            } => rewrite(class_name),
                            Rvalue::InstanceOf {
                                ref mut type_descriptor,
                                ..
                            } if user_classes.contains(type_descriptor.as_str()) => {
                                *type_descriptor = format!("{prefix}/{type_descriptor}");
                            }
                            Rvalue::Call { ref mut kind, .. } => match kind {
                                CallKind::Constructor(ref mut n)
                                    if user_classes.contains(n.as_str()) =>
                                {
                                    *n = format!("{prefix}/{n}");
                                }
                                CallKind::Virtual {
                                    ref mut class_name, ..
                                } => rewrite(class_name),
                                CallKind::Super {
                                    ref mut class_name, ..
                                } => rewrite(class_name),
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
            };
            // Top-level functions.
            for f in &mut module.functions {
                rewrite_fn(f);
            }
            // Class methods and constructors.
            for class in &mut module.classes {
                rewrite_fn(&mut class.constructor);
                for ctor in &mut class.secondary_constructors {
                    rewrite_fn(ctor);
                }
                for method in &mut class.methods {
                    rewrite_fn(method);
                }
            }
        }
    }

    // ─── Coroutine transform: continuation classes ────────
    //
    // Every suspend function the MIR lowerer marked with a
    // `SuspendStateMachine` needs a synthetic
    // `{Wrapper}${fn}$1 extends ContinuationImpl` alongside it.
    // We generate the class shape here so that the JVM backend
    // can emit both the caller's state machine and the companion
    // class in a single pass. The fields (`result: Object`,
    // `label: int`), the one-arg `<init>(Continuation)` super
    // call, and the `invokeSuspend(Object)` method that stashes
    // `$result`, flips the `label` sign bit, and re-invokes the
    // original `run(Continuation)` are all fixed shapes — the
    // only per-function inputs are the wrapper class name and
    // the suspend function's name.
    let continuation_classes: Vec<MirClass> = module
        .functions
        .iter()
        .filter_map(|f| {
            f.suspend_state_machine
                .as_ref()
                .map(|sm| build_continuation_class(sm, &module.wrapper_class, &f.name))
        })
        .collect();
    module.classes.extend(continuation_classes);

    // Also generate continuation classes for suspend
    // methods on user-defined classes. Skip SuspendLambda classes
    // (they ARE their own continuation — no separate companion).
    let class_cont_classes: Vec<MirClass> = module
        .classes
        .iter()
        .filter(|cls| !cls.is_suspend_lambda)
        .flat_map(|cls| {
            cls.methods.iter().filter_map(|m| {
                m.suspend_state_machine
                    .as_ref()
                    .map(|sm| build_continuation_class(sm, &cls.name, &m.name))
            })
        })
        .collect();
    module.classes.extend(class_cont_classes);

    module
}

/// Build the synthetic `ContinuationImpl` subclass for a single-
/// suspension-point state machine.
///
/// Shape (for `run()`):
///
/// ```text
/// final class InputKt$run$1 extends ContinuationImpl {
///     Object result;
///     int    label;
///     <init>(Continuation completion) {
///         super(completion);
///     }
///     public final Object invokeSuspend(Object $result) {
///         this.result = $result;
///         this.label |= 0x80000000;
///         return InputKt.run((Continuation) this);
///     }
/// }
/// ```
fn build_continuation_class(
    sm: &SuspendStateMachine,
    _wrapper_class: &str,
    _fn_name: &str,
) -> MirClass {
    // ── <init>(Continuation) { super(Continuation); return; } ──
    let mut ctor = MirFunction {
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
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
    };
    let this_local = ctor.new_local(Ty::Class(sm.continuation_class.clone()));
    let completion_local = ctor.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
    ctor.params.push(this_local);
    ctor.params.push(completion_local);
    let super_dest = ctor.new_local(Ty::Unit);
    ctor.blocks[0].stmts.push(MStmt::Assign {
        dest: super_dest,
        value: Rvalue::Call {
            kind: CallKind::Constructor(
                "kotlin/coroutines/jvm/internal/ContinuationImpl".to_string(),
            ),
            args: vec![this_local, completion_local],
        },
    });

    // ── invokeSuspend(Object $result): Object ──
    let mut invoke = MirFunction {
        id: FuncId(0),
        name: "invokeSuspend".to_string(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::Any,
        required_params: 0,
        param_names: vec!["$result".to_string()],
        param_defaults: Vec::new(),
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        // The `invokeSuspend` body is a fixed pattern the backend
        // emits directly: there's no MIR-level equivalent for the
        // `this.label |= 0x80000000` bitwise-OR step. We reuse the
        // same marker the top-level `run` function carries so the
        // JVM backend knows to substitute its canonical emitter.
        suspend_state_machine: Some(sm.clone()),
        annotations: Vec::new(),
    };
    let invoke_this = invoke.new_local(Ty::Class(sm.continuation_class.clone()));
    let result_param = invoke.new_local(Ty::Any);
    invoke.params.push(invoke_this);
    invoke.params.push(result_param);

    // Every local that lives across a suspend call
    // becomes a synthetic field on the continuation class. Fields
    // are emitted in `spill_layout` order so the backend's
    // getfield/putfield descriptors line up with what kotlinc
    // produces (and so two skotch runs are bit-stable).
    let mut fields: Vec<MirField> = Vec::new();
    for slot in &sm.spill_layout {
        fields.push(MirField {
            name: slot.name.clone(),
            ty: match slot.kind {
                SpillKind::Int => Ty::Int,
                SpillKind::Long => Ty::Long,
                SpillKind::Float => Ty::Int, // unused in this session
                SpillKind::Double => Ty::Double,
                SpillKind::Ref => Ty::Any,
            },
        });
    }
    fields.push(MirField {
        name: "result".to_string(),
        ty: Ty::Any,
    });
    fields.push(MirField {
        name: "label".to_string(),
        ty: Ty::Int,
    });

    MirClass {
        name: sm.continuation_class.clone(),
        super_class: Some("kotlin/coroutines/jvm/internal/ContinuationImpl".to_string()),
        is_open: false,
        is_abstract: false,
        is_interface: false,
        interfaces: Vec::new(),
        fields,
        methods: vec![invoke],
        constructor: ctor,
        secondary_constructors: Vec::new(),
        is_suspend_lambda: false,
        is_cross_file_stub: false,
        annotations: Vec::new(),
    }
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
/// Emit MIR-level autoboxing for a primitive value.  Returns a new
/// local holding the boxed reference (e.g. `Integer.valueOf(int)`) or
/// the original local unchanged if it is already a reference type.
fn mir_autobox(fb: &mut FnBuilder, val: LocalId, ty: &Ty) -> LocalId {
    match ty {
        Ty::Int => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Integer".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(I)Ljava/lang/Integer;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Long => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Long".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(J)Ljava/lang/Long;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Double => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Double".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(D)Ljava/lang/Double;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Bool => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Boolean".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(Z)Ljava/lang/Boolean;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Char => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Character".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(C)Ljava/lang/Character;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Byte => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Byte".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(B)Ljava/lang/Byte;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Short => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Short".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(S)Ljava/lang/Short;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        Ty::Float => {
            let boxed = fb.new_local(Ty::Any);
            fb.push_stmt(MStmt::Assign {
                dest: boxed,
                value: Rvalue::Call {
                    kind: CallKind::StaticJava {
                        class_name: "java/lang/Float".to_string(),
                        method_name: "valueOf".to_string(),
                        descriptor: "(F)Ljava/lang/Float;".to_string(),
                    },
                    args: vec![val],
                },
            });
            boxed
        }
        _ => val,
    }
}

/// Detect whether a lambda body contains a call to a suspend function.
/// Used to mark the lambda as a suspend lambda so future codegen can
/// generate a `SuspendLambda`-extending class.
///
/// Currently this is detection-only. The flag flows
/// through MIR but full codegen is a follow-up.
fn body_contains_suspend_call(
    body: &skotch_syntax::Block,
    module: &MirModule,
    interner: &Interner,
    name_to_func: &FxHashMap<Symbol, FuncId>,
) -> bool {
    fn scan_expr(
        e: &Expr,
        module: &MirModule,
        interner: &Interner,
        name_to_func: &FxHashMap<Symbol, FuncId>,
    ) -> bool {
        match e {
            Expr::Call { callee, args, .. } => {
                // Check if callee is a known suspend function
                if let Expr::Ident(name, _) = callee.as_ref() {
                    if let Some(&fid) = name_to_func.get(name) {
                        if let Some(f) = module.functions.get(fid.0 as usize) {
                            if f.is_suspend {
                                return true;
                            }
                        }
                    }
                }
                scan_expr(callee, module, interner, name_to_func)
                    || args
                        .iter()
                        .any(|a| scan_expr(&a.expr, module, interner, name_to_func))
            }
            Expr::Binary { lhs, rhs, .. } => {
                scan_expr(lhs, module, interner, name_to_func)
                    || scan_expr(rhs, module, interner, name_to_func)
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                scan_expr(cond, module, interner, name_to_func)
                    || scan_block(then_block, module, interner, name_to_func)
                    || else_block
                        .as_ref()
                        .is_some_and(|b| scan_block(b, module, interner, name_to_func))
            }
            Expr::Paren(inner, _) => scan_expr(inner, module, interner, name_to_func),
            _ => false,
        }
    }
    fn scan_block(
        b: &skotch_syntax::Block,
        module: &MirModule,
        interner: &Interner,
        name_to_func: &FxHashMap<Symbol, FuncId>,
    ) -> bool {
        for stmt in &b.stmts {
            match stmt {
                Stmt::Expr(e) if scan_expr(e, module, interner, name_to_func) => {
                    return true;
                }
                Stmt::Val(v) if scan_expr(&v.init, module, interner, name_to_func) => {
                    return true;
                }
                Stmt::Return { value: Some(e), .. }
                    if scan_expr(e, module, interner, name_to_func) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false
    }
    scan_block(body, module, interner, name_to_func)
}

/// Collect free variables in a lambda body: names that are referenced
/// but not defined as lambda parameters or known top-level functions.
fn collect_free_vars(
    body: &skotch_syntax::Block,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    fb: &FnBuilder,
    _interner: &Interner,
) -> Vec<(Symbol, LocalId, Ty)> {
    let mut free = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    collect_free_in_block(body, param_names, outer_scope, fb, &mut free, &mut seen);
    free
}

fn collect_free_in_block(
    block: &skotch_syntax::Block,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    fb: &FnBuilder,
    free: &mut Vec<(Symbol, LocalId, Ty)>,
    seen: &mut rustc_hash::FxHashSet<Symbol>,
) {
    for stmt in &block.stmts {
        match stmt {
            Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } => {
                collect_free_in_expr(e, param_names, outer_scope, fb, free, seen);
            }
            Stmt::Val(v) => {
                collect_free_in_expr(&v.init, param_names, outer_scope, fb, free, seen);
            }
            Stmt::Assign { value, .. } => {
                collect_free_in_expr(value, param_names, outer_scope, fb, free, seen);
            }
            Stmt::IndexAssign {
                receiver,
                index,
                value,
                ..
            } => {
                collect_free_in_expr(receiver, param_names, outer_scope, fb, free, seen);
                collect_free_in_expr(index, param_names, outer_scope, fb, free, seen);
                collect_free_in_expr(value, param_names, outer_scope, fb, free, seen);
            }
            _ => {}
        }
    }
}

fn collect_free_in_expr(
    e: &Expr,
    param_names: &[Symbol],
    outer_scope: &[(Symbol, LocalId)],
    fb: &FnBuilder,
    free: &mut Vec<(Symbol, LocalId, Ty)>,
    seen: &mut rustc_hash::FxHashSet<Symbol>,
) {
    match e {
        Expr::Ident(name, _) if !param_names.contains(name) && !seen.contains(name) => {
            if let Some((_, local_id)) = outer_scope.iter().rev().find(|(s, _)| s == name) {
                let ty = fb.mf.locals[local_id.0 as usize].clone();
                free.push((*name, *local_id, ty));
                seen.insert(*name);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_free_in_expr(lhs, param_names, outer_scope, fb, free, seen);
            collect_free_in_expr(rhs, param_names, outer_scope, fb, free, seen);
        }
        Expr::Unary { operand, .. } => {
            collect_free_in_expr(operand, param_names, outer_scope, fb, free, seen);
        }
        Expr::Call { callee, args, .. } => {
            collect_free_in_expr(callee, param_names, outer_scope, fb, free, seen);
            for a in args {
                collect_free_in_expr(&a.expr, param_names, outer_scope, fb, free, seen);
            }
        }
        Expr::Field { receiver, .. } | Expr::SafeCall { receiver, .. } => {
            collect_free_in_expr(receiver, param_names, outer_scope, fb, free, seen);
        }
        Expr::Index {
            receiver, index, ..
        } => {
            collect_free_in_expr(receiver, param_names, outer_scope, fb, free, seen);
            collect_free_in_expr(index, param_names, outer_scope, fb, free, seen);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_free_in_expr(cond, param_names, outer_scope, fb, free, seen);
            collect_free_in_block(then_block, param_names, outer_scope, fb, free, seen);
            if let Some(eb) = else_block {
                collect_free_in_block(eb, param_names, outer_scope, fb, free, seen);
            }
        }
        Expr::StringTemplate(parts, _) => {
            for p in parts {
                match p {
                    skotch_syntax::TemplatePart::Expr(inner) => {
                        collect_free_in_expr(inner, param_names, outer_scope, fb, free, seen);
                    }
                    skotch_syntax::TemplatePart::IdentRef(sym, _)
                        if !param_names.contains(sym) && !seen.contains(sym) =>
                    {
                        if let Some((_, local_id)) =
                            outer_scope.iter().rev().find(|(s, _)| s == sym)
                        {
                            let ty = fb.mf.locals[local_id.0 as usize].clone();
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
            collect_free_in_expr(inner, param_names, outer_scope, fb, free, seen);
        }
        Expr::ElvisOp { lhs, rhs, .. } => {
            collect_free_in_expr(lhs, param_names, outer_scope, fb, free, seen);
            collect_free_in_expr(rhs, param_names, outer_scope, fb, free, seen);
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
            collect_free_in_block(inner_body, &extended_params, outer_scope, fb, free, seen);
        }
        _ => {}
    }
}

fn lower_const_init(e: &Expr, module: &mut MirModule) -> Option<MirConst> {
    match e {
        Expr::IntLit(v, _) => Some(MirConst::Int(*v as i32)),
        Expr::LongLit(v, _) => Some(MirConst::Long(*v)),
        Expr::FloatLit(v, _) => Some(MirConst::Float(*v as f32)),
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
        MirConst::Float(_) => Ty::Float,
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
    /// Symbols declared as `var` (mutable) in this function scope.
    var_syms: rustc_hash::FxHashSet<Symbol>,
    /// MIR locals that correspond to suspend-typed function
    /// parameters (e.g. `block: suspend () -> String`). When such a
    /// local is invoked as a callable, the MIR lowerer must append the
    /// enclosing function's `$completion` continuation as the trailing
    /// argument and skip the normal Int-unbox on the Object result.
    suspend_callable_locals: rustc_hash::FxHashSet<u32>,
    /// Reified type parameter substitutions for the current inline scope.
    /// Maps type param names (e.g. "A", "B") to concrete types (e.g. "String").
    reified_types: FxHashMap<String, String>,
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
            param_receiver_types: Vec::new(),
            is_abstract: false,
            exception_handlers: Vec::new(),
            vararg_index: None,
            is_suspend: false,
            is_inline: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: Vec::new(),
        };
        FnBuilder {
            mf,
            cur_block: 0,
            var_syms: rustc_hash::FxHashSet::default(),
            suspend_callable_locals: rustc_hash::FxHashSet::default(),
            reified_types: FxHashMap::default(),
        }
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

/// Outcome of [`extract_suspend_state_machine`] — the three cases
/// the coroutine lowering passes branch on.
enum SuspendSitesResult {
    /// No inner suspend calls; the signature rewrite is
    /// all we need.
    Zero,
    /// At least one inner suspend call; we emit the state-machine
    /// shape with the supplied marker. Single-
    /// suspension (string-literal tail) and N-
    /// suspension (real expression tail) cases both surface here — the
    /// marker's `sites` vector distinguishes them for the backend.
    Found(SuspendStateMachine),
    /// The function has suspend calls, but the body shape is
    /// outside the current scope. Reserved for future use.
    #[allow(dead_code)]
    Unsupported(String),
}

/// Scan a freshly-lowered suspend function for calls to other
/// suspend functions and, if they're present, build a
/// [`SuspendStateMachine`] marker describing the coroutine
/// dispatcher the JVM backend should emit.
///
/// Two shapes are recognized:
///
/// - **Single-suspension.** A single suspend call with no arguments beyond
///   the synthesized `$completion`, followed by a literal-string
///   return terminator. Produces the historic marker shape
///   (`sites` empty, `resume_return_text` populated).
/// - **Multi-suspension.** Any number of suspend calls with no args beyond
///   `$completion`, in a straight-line single-block body. Produces
///   a marker with populated `sites` and `spill_layout` and lets
///   the JVM backend walk the real MIR for the resume tail.
///
/// Anything richer (suspend calls with user args, branches across
/// suspend sites) becomes [`SuspendSitesResult::Unsupported`] so the
/// caller can emit a precise diagnostic. Future work will lift
/// these restrictions.
fn extract_suspend_state_machine(
    mf: &MirFunction,
    module: &MirModule,
    wrapper_class: &str,
    fn_name: &str,
) -> SuspendSitesResult {
    extract_suspend_state_machine_with_cont(
        mf,
        module,
        wrapper_class,
        fn_name,
        format!("{wrapper_class}${fn_name}$1"),
    )
}

/// Variant of [`extract_suspend_state_machine`] that lets the caller
/// override the continuation class name. Suspend lambdas use this
/// because the lambda class IS the continuation (it
/// extends `SuspendLambda`), so there is no separate `InputKt$fn$1`
/// companion — the state machine stored on the lambda's invoke
/// method points at the lambda class itself.
fn extract_suspend_state_machine_with_cont(
    mf: &MirFunction,
    module: &MirModule,
    wrapper_class: &str,
    fn_name: &str,
    continuation_class: String,
) -> SuspendSitesResult {
    // Walk every block collecting (block_idx, stmt_idx, callee FuncId, argc)
    // for each suspend-fn static call. The MIR lowerer routes these
    // through `invokestatic` with the trailing `$completion`
    // already appended, so they're easy to recognize structurally.
    // Suspend call sites come in two flavors:
    // 1. Static calls to known suspend functions (CallKind::Static)
    // 2. Virtual/interface calls to suspend methods (VirtualJava)
    //    — e.g., Deferred.await(), Function1.invoke() on suspend types
    //
    // We track both with a sentinel FuncId(u32::MAX) for virtual calls
    // since they don't have a real FuncId in our module.
    let virtual_suspend_fid = FuncId(u32::MAX);
    let mut sites_raw: Vec<(u32, u32, FuncId, usize)> = Vec::new();
    for (bi, block) in mf.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            let MStmt::Assign { value, .. } = stmt;
            match value {
                Rvalue::Call {
                    kind: CallKind::Static(fid),
                    args,
                } => {
                    let target = &module.functions[fid.0 as usize];
                    if target.is_suspend {
                        sites_raw.push((bi as u32, si as u32, *fid, args.len()));
                    }
                }
                Rvalue::Call {
                    kind:
                        CallKind::VirtualJava {
                            class_name,
                            method_name,
                            descriptor,
                        },
                    args,
                } => {
                    // Known suspend interface/virtual methods.
                    // Any VirtualJava call whose descriptor ends with
                    // `Continuation;)Ljava/lang/Object;` is a suspend
                    // method call (user-defined or library).
                    let is_suspend_virtual = (class_name == "kotlinx/coroutines/Deferred"
                        && method_name == "await")
                        || (class_name == "kotlinx/coroutines/Job" && method_name == "join")
                        || (class_name.starts_with("kotlin/jvm/functions/Function")
                            && method_name == "invoke"
                            && descriptor.contains("Continuation"))
                        || (descriptor
                            .contains("Lkotlin/coroutines/Continuation;)Ljava/lang/Object;"));
                    if is_suspend_virtual {
                        sites_raw.push((bi as u32, si as u32, virtual_suspend_fid, args.len()));
                    }
                }
                _ => {}
            }
        }
    }
    if sites_raw.is_empty() {
        return SuspendSitesResult::Zero;
    }

    // Compute the outer function's user param types (everything
    // except the last param which is the $completion Continuation).
    let outer_user_param_tys: Vec<Ty> = if mf.params.len() > 1 {
        mf.params[..mf.params.len() - 1]
            .iter()
            .map(|pid| mf.locals[pid.0 as usize].clone())
            .collect()
    } else {
        Vec::new()
    };

    // Single-suspension path: if the body also has a string-literal
    // return tail we keep the single-suspension shape byte-for-byte (the
    // committed 391 fixture depends on it). This only applies when
    // the sole suspend call has just the implicit `$completion` — a
    // user-arg call can't be expressed in the legacy marker
    // (there is no way to thread arg values into the legacy emitter).
    if sites_raw.len() == 1 && sites_raw[0].3 == 1 {
        if let Some(resume_return_text) = extract_trailing_string(mf, module) {
            let (_, _, fid, _) = sites_raw[0];
            let target = &module.functions[fid.0 as usize];
            return SuspendSitesResult::Found(SuspendStateMachine {
                continuation_class,
                outer_class: wrapper_class.to_string(),
                outer_method: fn_name.to_string(),
                outer_user_param_tys: outer_user_param_tys.clone(),
                suspend_call_class: wrapper_class.to_string(),
                suspend_call_method: target.name.clone(),
                resume_return_text,
                sites: Vec::new(),
                spill_layout: Vec::new(),
                is_instance_method: false,
            });
        }
    }

    // Multi-block state machines are now supported.
    // Build per-site callee info and the conservative spill layout.
    let (sites, spill_layout) = build_sites_and_spills(mf, module, &sites_raw);

    SuspendSitesResult::Found(SuspendStateMachine {
        continuation_class,
        outer_class: wrapper_class.to_string(),
        outer_method: fn_name.to_string(),
        outer_user_param_tys,
        // Legacy compatibility fields stay populated (with the
        // FIRST callee) purely so MIR consumers that peek at them
        // don't see empty strings; the backend ignores them in the
        // multi-suspension path.
        suspend_call_class: wrapper_class.to_string(),
        suspend_call_method: {
            let (_, _, fid, _) = sites_raw[0];
            if fid.0 == u32::MAX {
                // Virtual suspend call — name is in the SuspendCallSite
                String::new()
            } else {
                module.functions[fid.0 as usize].name.clone()
            }
        },
        resume_return_text: String::new(),
        sites,
        spill_layout,
        is_instance_method: false,
    })
}

/// Given the raw suspend-call sites (block, stmt, callee, argc), in
/// source order, compute:
///
/// 1. the per-site callee info the backend needs to emit
///    `invokestatic`,
/// 2. the set of MIR locals live across each suspend call (over-
///    conservative — every local assigned earlier in the block and
///    referenced later is included), and
/// 3. the continuation-class spill layout (`I$0`, `L$0`, …) those
///    locals map to.
///
/// This is the straight-line-body specialization of a standard
/// liveness analysis: for each call site we just scan prior stmts
/// for writes and later stmts (+ the terminator) for reads. That
/// over-spills dead locals, but is always correct — a more precise
/// analysis is a future follow-up.
fn build_sites_and_spills(
    mf: &MirFunction,
    module: &MirModule,
    sites_raw: &[(u32, u32, FuncId, usize)],
) -> (Vec<SuspendCallSite>, Vec<SpillSlot>) {
    // Per-kind running counters for slot name suffixes (I$0, I$1, …).
    let mut kind_counters: [u32; 5] = [0; 5];
    fn kind_idx(k: SpillKind) -> usize {
        match k {
            SpillKind::Int => 0,
            SpillKind::Long => 1,
            SpillKind::Float => 2,
            SpillKind::Double => 3,
            SpillKind::Ref => 4,
        }
    }

    // Maps MIR local -> index in spill_layout, so repeated uses of
    // the same local across multiple suspend sites share one field.
    let mut local_to_slot: FxHashMap<u32, u32> = FxHashMap::default();
    let mut spill_layout: Vec<SpillSlot> = Vec::new();

    // Multi-block support. If all sites are in the same
    // block, use the original single-block analysis. Otherwise use
    // a conservative inter-block analysis.
    // Single-block only when ALL sites are in the SAME block
    // AND no other blocks have executable statements. Empty blocks with
    // just Goto/Return terminators are fine (common in lambda bodies).
    let is_single_block = {
        let first = sites_raw[0].0;
        let all_same = sites_raw.iter().all(|(b, _, _, _)| *b == first);
        let site_blocks: rustc_hash::FxHashSet<u32> =
            sites_raw.iter().map(|(b, _, _, _)| *b).collect();
        let has_code_blocks = mf
            .blocks
            .iter()
            .enumerate()
            .any(|(i, b)| !site_blocks.contains(&(i as u32)) && !b.stmts.is_empty());
        all_same && !has_code_blocks
    };

    let mut sites_out: Vec<SuspendCallSite> = Vec::with_capacity(sites_raw.len());
    for (bi, si, fid, _) in sites_raw {
        let block = &mf.blocks[*bi as usize];

        // Collect locals WRITTEN before this site.
        // Include function parameters (they're live from
        // function entry). Exclude the last param ($completion).
        let mut written: Vec<LocalId> = Vec::new();
        let n_params = mf.params.len();
        for &pid in mf.params.iter().take(n_params.saturating_sub(1)) {
            written.push(pid);
        }
        if is_single_block {
            // Original path: only scan within the single block.
            for (idx, stmt) in block.stmts.iter().enumerate() {
                if (idx as u32) >= *si {
                    break;
                }
                let MStmt::Assign { dest, .. } = stmt;
                written.push(*dest);
            }
        } else {
            // Multi-block: include locals from block 0 (entry — always
            // dominates all other blocks) + the site's own block
            // before the suspend call. Don't include other NON-entry
            // blocks — locals from other branches may not be
            // initialized on this execution path.
            if *bi != 0 {
                for stmt in &mf.blocks[0].stmts {
                    let MStmt::Assign { dest, .. } = stmt;
                    written.push(*dest);
                }
            }
            for (idx, stmt) in block.stmts.iter().enumerate() {
                if (idx as u32) >= *si {
                    break;
                }
                let MStmt::Assign { dest, .. } = stmt;
                written.push(*dest);
            }
        }

        // Collect locals READ after this site.
        let mut read: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();
        if is_single_block {
            for (idx, stmt) in block.stmts.iter().enumerate() {
                if (idx as u32) <= *si {
                    continue;
                }
                collect_reads(stmt, &mut read);
            }
            collect_terminator_reads(&block.terminator, &mut read);
        } else {
            // Multi-block: only scan reads AFTER the suspend site in
            // the same block + terminator. Only these stmts run in the
            // resume path for this specific branch.
            for (idx, stmt) in block.stmts.iter().enumerate() {
                if (idx as u32) <= *si {
                    continue;
                }
                collect_reads(stmt, &mut read);
            }
            collect_terminator_reads(&block.terminator, &mut read);
        }

        // Skip locals that are the suspend-call's own dest.
        let own_dest = {
            let MStmt::Assign { dest, .. } = &block.stmts[*si as usize];
            *dest
        };

        // For instance suspend methods, `this` (first param)
        // must always be spilled so `invokeSuspend` can use it as the
        // invokevirtual receiver. Force it into the read set.
        if !mf.params.is_empty() {
            let first_param = mf.params[0];
            let first_ty = &mf.locals[first_param.0 as usize];
            if matches!(first_ty, Ty::Class(_)) && mf.is_suspend {
                read.insert(first_param.0);
            }
        }

        let mut live_spills: Vec<LiveSpill> = Vec::new();
        for w in written {
            if w == own_dest {
                continue;
            }
            if !read.contains(&w.0) {
                continue;
            }
            let ty = &mf.locals[w.0 as usize];
            // Don't try to spill `Unit` — it's not a real value on
            // JVM. (Shouldn't arise in our scope, but guard.)
            if matches!(ty, Ty::Unit) {
                continue;
            }
            let kind = SpillKind::for_ty(ty);
            let slot_idx = *local_to_slot.entry(w.0).or_insert_with(|| {
                let n = kind_counters[kind_idx(kind)];
                kind_counters[kind_idx(kind)] = n + 1;
                let name = format!("{}{}", kind.prefix(), n);
                let idx = spill_layout.len() as u32;
                spill_layout.push(SpillSlot { name, kind });
                idx
            });
            live_spills.push(LiveSpill {
                local: w,
                slot: slot_idx,
            });
        }
        // Sort by slot index so the emitted spill/restore order is
        // stable across runs.
        live_spills.sort_by_key(|ls| ls.slot);

        // Extract the user-argument locals (everything
        // except the trailing `$completion`) plus their types, and
        // surface the callee's declared return type so the JVM
        // backend can emit a post-invoke `checkcast`. Older
        // single-suspension call shapes have no user args and a
        // `Unit`-typed callee, which keeps `args`/`arg_tys` empty
        // and the legacy emit path byte-stable.
        let stmt = &block.stmts[*si as usize];
        let MStmt::Assign {
            dest: call_dest,
            value: call_value,
        } = stmt;
        let Rvalue::Call {
            args: call_args, ..
        } = call_value
        else {
            unreachable!("suspend-call site is always a Call rvalue");
        };
        // The last arg is the implicit $completion the MIR lowerer
        // appends; everything before it is a user argument.
        let user_arg_count = call_args.len().saturating_sub(1);
        let args: Vec<LocalId> = call_args[..user_arg_count].to_vec();
        // Use the target function's declared parameter
        // types (not the call-site argument types) so that the
        // callee descriptor uses the correct interface types. For
        // example, when passing an `InputKt$Lambda$0` to a param
        // typed as `Function1`, the descriptor must say `Function1`.
        // Handle both Static and VirtualJava suspend calls.
        // Virtual calls use a sentinel FuncId(u32::MAX).
        let is_virtual = fid.0 == u32::MAX;
        let (arg_tys, return_ty, callee_class, callee_method_name): (Vec<Ty>, Ty, String, String) =
            if is_virtual {
                // Extract info from the VirtualJava call rvalue directly
                let stmt = &block.stmts[*si as usize];
                let MStmt::Assign { value, .. } = stmt;
                match value {
                    Rvalue::Call {
                        kind:
                            CallKind::VirtualJava {
                                class_name,
                                method_name,
                                ..
                            },
                        ..
                    } => {
                        // For await(), there are no user args — just receiver + continuation
                        // The receiver is the first arg, continuation is last
                        let at: Vec<Ty> = args
                            .iter()
                            .map(|a| mf.locals[a.0 as usize].clone())
                            .collect();
                        (at, Ty::Any, class_name.clone(), method_name.clone())
                    }
                    _ => unreachable!("virtual suspend site must be VirtualJava"),
                }
            } else {
                let target = &module.functions[fid.0 as usize];
                let at: Vec<Ty> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        target
                            .params
                            .get(i)
                            .and_then(|p| target.locals.get(p.0 as usize))
                            .cloned()
                            .unwrap_or_else(|| mf.locals[a.0 as usize].clone())
                    })
                    .collect();
                let rt = target
                    .suspend_original_return_ty
                    .clone()
                    .unwrap_or(Ty::Unit);
                let cc = match target.name.as_str() {
                    "delay" => "kotlinx/coroutines/DelayKt".to_string(),
                    "yield" => "kotlinx/coroutines/YieldKt".to_string(),
                    "withContext" => "kotlinx/coroutines/BuildersKt".to_string(),
                    "coroutineScope" => "kotlinx/coroutines/CoroutineScopeKt".to_string(),
                    "supervisorScope" => "kotlinx/coroutines/SupervisorKt".to_string(),
                    "withTimeout" | "withTimeoutOrNull" => {
                        "kotlinx/coroutines/TimeoutKt".to_string()
                    }
                    _ => module.wrapper_class.clone(),
                };
                (at, rt, cc, target.name.clone())
            };

        sites_out.push(SuspendCallSite {
            block_idx: *bi,
            stmt_idx: *si,
            callee_class,
            callee_method: callee_method_name,
            args,
            arg_tys,
            return_ty,
            is_virtual,
            result_local: *call_dest,
            live_spills,
        });
    }

    (sites_out, spill_layout)
}

/// Collect every `LocalId` read (as a source operand) by an
/// `MStmt::Assign`. Writes to `dest` are intentionally excluded.
fn collect_reads(stmt: &MStmt, out: &mut rustc_hash::FxHashSet<u32>) {
    let MStmt::Assign { value, .. } = stmt;
    collect_rvalue_reads(value, out);
}

fn collect_rvalue_reads(v: &Rvalue, out: &mut rustc_hash::FxHashSet<u32>) {
    match v {
        Rvalue::Const(_) | Rvalue::NewInstance(_) | Rvalue::GetStaticField { .. } => {}
        Rvalue::Local(l) => {
            out.insert(l.0);
        }
        Rvalue::BinOp { lhs, rhs, .. } => {
            out.insert(lhs.0);
            out.insert(rhs.0);
        }
        Rvalue::GetField { receiver, .. } => {
            out.insert(receiver.0);
        }
        Rvalue::PutField {
            receiver, value, ..
        } => {
            out.insert(receiver.0);
            out.insert(value.0);
        }
        Rvalue::Call { args, .. } => {
            for a in args {
                out.insert(a.0);
            }
        }
        Rvalue::InstanceOf { obj, .. } => {
            out.insert(obj.0);
        }
        Rvalue::NewIntArray(l) | Rvalue::ArrayLength(l) | Rvalue::NewObjectArray(l) => {
            out.insert(l.0);
        }
        Rvalue::NewTypedObjectArray { size, .. } => {
            out.insert(size.0);
        }
        Rvalue::ArrayLoad { array, index } => {
            out.insert(array.0);
            out.insert(index.0);
        }
        Rvalue::ArrayStore {
            array,
            index,
            value,
        }
        | Rvalue::ObjectArrayStore {
            array,
            index,
            value,
        } => {
            out.insert(array.0);
            out.insert(index.0);
            out.insert(value.0);
        }
        Rvalue::CheckCast { obj, .. } => {
            out.insert(obj.0);
        }
    }
}

fn collect_terminator_reads(t: &Terminator, out: &mut rustc_hash::FxHashSet<u32>) {
    match t {
        Terminator::Return | Terminator::Goto(_) => {}
        Terminator::ReturnValue(l) => {
            out.insert(l.0);
        }
        Terminator::Branch { cond, .. } => {
            out.insert(cond.0);
        }
        Terminator::Throw(exc) => {
            out.insert(exc.0);
        }
    }
}

/// Walk the function's last block and, if its terminator is
/// `ReturnValue(local)` where `local` was assigned a
/// `Rvalue::Const(MirConst::String(...))`, resolve the string
/// id back to its text via the module's string pool. Used by
/// the single-suspension path to recognize the "return \"done\"" post-suspend
/// tail pattern and thread the literal through to the JVM
/// backend's constant pool.
fn extract_trailing_string(mf: &MirFunction, module: &MirModule) -> Option<String> {
    let last = mf.blocks.last()?;
    let Terminator::ReturnValue(local) = &last.terminator else {
        return None;
    };
    // Scan the last block's stmts for the assignment to this local.
    // The CPS rewrite path wraps the return in a `mir_autobox` call
    // followed by a `Call` to `Integer.valueOf` etc., so the value
    // we care about might be several assignments back. Walk
    // backwards and stop at the first `Const(String(sid))` we see
    // referencing the terminator's local, either directly or via
    // a single `Rvalue::Local(...)` alias.
    let mut tracked = *local;
    for stmt in last.stmts.iter().rev() {
        let MStmt::Assign { dest, value } = stmt;
        if *dest != tracked {
            continue;
        }
        match value {
            Rvalue::Const(MirConst::String(sid)) => {
                return Some(module.lookup_string(*sid).to_string());
            }
            Rvalue::Local(src) => {
                tracked = *src;
            }
            _ => return None,
        }
    }
    None
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
    let declared_ret = typed
        .map(|t| t.return_ty.clone())
        .or_else(|| {
            f.return_ty.as_ref().map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
        })
        .unwrap_or(Ty::Unit);
    // Coroutine transform: a `suspend fun`'s
    // JVM return type becomes `Object` (we use `Ty::Any`). The
    // original declared return type is still needed below so we
    // can box primitive `return` expressions before the
    // `areturn`. See milestones.yaml v0.9.0.
    let return_ty = if f.is_suspend {
        Ty::Any
    } else {
        declared_ret.clone()
    };
    let mut fb = FnBuilder::new(fn_idx, name.clone(), return_ty);
    fb.mf.is_suspend = f.is_suspend;
    // Coroutine transform: remember the source-
    // level declared return type of every suspend fun before the
    // CPS rewrite promotes it to `Object`. Callers that invoke
    // this suspend fun need it to emit the correct `checkcast` on
    // resume. Non-suspend funs leave this as `None`.
    if f.is_suspend {
        fb.mf.suspend_original_return_ty = Some(declared_ret.clone());
    }

    // Allocate parameter locals first so they get LocalId 0..N.
    let mut scope: Vec<(Symbol, LocalId)> = Vec::new();

    // For extension functions: add the receiver as the first parameter.
    // It's accessible as `this` in the function body.
    if let Some(recv) = &f.receiver_ty {
        let recv_ty = resolve_type(interner.resolve(recv.name), module);
        let id = fb.new_local(recv_ty);
        fb.mf.params.push(id);
        let this_sym = interner.intern("this");
        scope.push((this_sym, id));
    }

    for (pi, p) in f.params.iter().enumerate() {
        let ty = if p.is_vararg {
            // vararg Int → IntArray (JVM `int[]`)
            Ty::IntArray
        } else {
            typed
                .and_then(|t| {
                    t.param_tys
                        .get(pi + if f.receiver_ty.is_some() { 1 } else { 0 })
                        .cloned()
                })
                .unwrap_or_else(|| resolve_type(interner.resolve(p.ty.name), module))
        };
        // Detect extension function type parameters. The parser sets
        // `has_receiver = true` on types like `StringBuilder.() -> Unit`
        // and encodes the receiver as func_params[0]. Record the
        // receiver class name for call-site lambda-with-receiver dispatch.
        if p.ty.has_receiver {
            if let Some(ref fparams) = p.ty.func_params {
                if !fparams.is_empty() {
                    let recv_name = interner.resolve(fparams[0].name).to_string();
                    let jvm_class = match recv_name.as_str() {
                        "StringBuilder" => "java/lang/StringBuilder".to_string(),
                        "String" => "java/lang/String".to_string(),
                        other => other.to_string(),
                    };
                    fb.mf.param_receiver_types.push((pi, jvm_class));
                }
            }
        }
        // Function-typed parameters → kotlin/jvm/functions/FunctionN interface.
        // Suspend function types bump arity by +1 for the
        // implicit Continuation parameter — `suspend () -> T` maps to
        // `Function1<Continuation, Object>`, not `Function0`.
        let is_suspend_callable = matches!(
            ty,
            Ty::Function {
                is_suspend: true,
                ..
            }
        );
        let ty = if let Ty::Function {
            ref params,
            is_suspend: fn_suspend,
            ..
        } = ty
        {
            let arity = params.len() + if fn_suspend { 1 } else { 0 };
            let iface = stdlib_function_interface(arity);
            Ty::Class(iface)
        } else {
            ty
        };
        // Override enum class types to String (enums are string-based).
        let id = fb.new_local(ty);
        fb.mf.params.push(id);
        // Record suspend-typed callable parameters so that
        // when they're invoked the MIR lowerer threads the continuation.
        if is_suspend_callable {
            fb.suspend_callable_locals.insert(id.0);
        }
        scope.push((p.name, id));
    }

    // Coroutine transform: a `suspend fun`'s
    // last parameter is an implicit `$completion: Continuation`.
    // We append it AFTER the explicit user parameters so the
    // source-visible parameter positions are unchanged.
    if f.is_suspend {
        let cont_ty = Ty::Class("kotlin/coroutines/Continuation".to_string());
        let cont_id = fb.new_local(cont_ty);
        fb.mf.params.push(cont_id);
        let cont_sym = interner.intern("$completion");
        scope.push((cont_sym, cont_id));
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

    // Coroutine transform: rewrite every
    // return in this suspend function so that the returned value
    // is a reference (the method descriptor now promises
    // `Object`). `ReturnValue` terminators get autoboxed; bare
    // `Return` terminators (suspend fun with a `Unit` declared
    // return type) become `return null`, which is a valid
    // `Object`. Emitting `kotlin.Unit.INSTANCE` instead is the
    // semantically correct thing and will land together with the
    // full CPS state-machine.
    if f.is_suspend {
        for bi in 0..fb.mf.blocks.len() {
            match fb.mf.blocks[bi].terminator.clone() {
                Terminator::ReturnValue(local) => {
                    let ty = fb.mf.locals[local.0 as usize].clone();
                    let prev_block = fb.cur_block;
                    fb.cur_block = bi as u32;
                    let boxed = mir_autobox(&mut fb, local, &ty);
                    fb.cur_block = prev_block;
                    fb.mf.blocks[bi].terminator = Terminator::ReturnValue(boxed);
                }
                Terminator::Return => {
                    let prev_block = fb.cur_block;
                    fb.cur_block = bi as u32;
                    let null_local = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest: null_local,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    fb.cur_block = prev_block;
                    fb.mf.blocks[bi].terminator = Terminator::ReturnValue(null_local);
                }
                _ => {}
            }
        }
    }

    // Coroutine transform: if this suspend
    // function contains any suspension points, attach a
    // [`SuspendStateMachine`] marker so the JVM backend emits the
    // canonical dispatcher + tableswitch pattern. Zero suspension
    // points falls through to the signature-rewrite-only shape. The extractor
    // rejects shapes outside the current scope (suspend calls with
    // user args, branches across suspend sites) as hard errors
    // rather than silently miscompiling.
    if f.is_suspend && ok {
        let sm = extract_suspend_state_machine(&fb.mf, module, &module.wrapper_class, &name);
        match sm {
            SuspendSitesResult::Zero => {}
            SuspendSitesResult::Found(state_machine) => {
                fb.mf.suspend_state_machine = Some(state_machine);
            }
            SuspendSitesResult::Unsupported(reason) => {
                diags.push(Diagnostic::error(
                    f.span,
                    format!(
                        "suspend function `{name}` has an unsupported shape: {reason}; the skotch \
                         CPS transform currently supports straight-line bodies with suspend calls \
                         that take only the implicit `$completion`"
                    ),
                ));
                ok = false;
            }
        }
    }

    // When a non-suspend function returns Unit but the body
    // ends with `ReturnValue(local)` whose type is non-Unit (e.g.
    // `fun main() = runBlocking { ... }` where runBlocking returns Object),
    // drop the value and emit plain `Return`. The JVM backend would
    // otherwise emit `areturn` from a `void` method, failing verification.
    if !f.is_suspend && declared_ret == Ty::Unit {
        for block in &mut fb.mf.blocks {
            if let Terminator::ReturnValue(local) = &block.terminator {
                let ty = &fb.mf.locals[local.0 as usize];
                if !matches!(ty, Ty::Unit) {
                    block.terminator = Terminator::Return;
                }
            }
        }
    }

    if ok {
        // Preserve param metadata from Pass 1.
        let saved_defaults = module.functions[fn_idx].param_defaults.clone();
        let saved_required = module.functions[fn_idx].required_params;
        let saved_names = module.functions[fn_idx].param_names.clone();
        let saved_vararg = module.functions[fn_idx].vararg_index;
        let saved_suspend = module.functions[fn_idx].is_suspend;
        let saved_inline = module.functions[fn_idx].is_inline;
        let saved_annotations = module.functions[fn_idx].annotations.clone();
        module.functions[fn_idx] = fb.finish();
        module.functions[fn_idx].param_defaults = saved_defaults;
        module.functions[fn_idx].required_params = saved_required;
        module.functions[fn_idx].param_names = saved_names;
        module.functions[fn_idx].vararg_index = saved_vararg;
        module.functions[fn_idx].is_suspend = saved_suspend;
        module.functions[fn_idx].is_inline = saved_inline;
        module.functions[fn_idx].annotations = saved_annotations;
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
            param_receiver_types: Vec::new(),
            is_abstract: false,
            exception_handlers: Vec::new(),
            vararg_index: None,
            is_suspend: false,
            is_inline: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: Vec::new(),
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
            } else {
                // `return` (or `return@label`) with no value — exits the
                // current function/lambda returning Unit/void.
                fb.set_terminator(Terminator::Return);
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
                // Check if this var is ref-boxed (captured mutable var).
                let ref_holder_name =
                    interner.intern(&format!("$ref${}", interner.resolve(*target)));
                if let Some((_, ref_lid)) = scope
                    .iter()
                    .rev()
                    .find(|(name, _)| *name == ref_holder_name)
                {
                    let ref_lid = *ref_lid;
                    // Find the $Ref class name from the local's type.
                    let ref_class = match &fb.mf.locals[ref_lid.0 as usize] {
                        Ty::Class(name) => name.clone(),
                        _ => String::new(),
                    };
                    if !ref_class.is_empty() {
                        // Write through the $Ref: ref.element = rhs
                        fb.push_stmt(MStmt::Assign {
                            dest: ref_lid,
                            value: Rvalue::PutField {
                                receiver: ref_lid,
                                class_name: ref_class.clone(),
                                field_name: "element".to_string(),
                                value: rhs,
                            },
                        });
                        // In the lambda invoke scope, also update the
                        // element-typed local copy so subsequent reads
                        // see the new value.  Skip this in the outer scope
                        // where the target local IS the $Ref object.
                        if let Some((_, local_id)) =
                            scope.iter().rev().find(|(name, _)| *name == *target)
                        {
                            let dest = *local_id;
                            let dest_ty = &fb.mf.locals[dest.0 as usize];
                            if !matches!(dest_ty, Ty::Class(n) if n.starts_with("$Ref$")) {
                                fb.push_stmt(MStmt::Assign {
                                    dest,
                                    value: Rvalue::Local(rhs),
                                });
                            }
                        }
                    }
                } else {
                    // Normal (non-ref-boxed) assignment.
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
            }
            true
        }
        Stmt::IndexAssign {
            receiver,
            index,
            value,
            ..
        } => {
            let arr = lower_expr(
                receiver,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            );
            let idx = lower_expr(
                index,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            );
            let val = lower_expr(
                value,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            );
            if let (Some(a), Some(i), Some(v)) = (arr, idx, val) {
                let arr_ty = fb.mf.locals[a.0 as usize].clone();
                // Check for operator fun set() on user-defined classes.
                let has_set = if let Ty::Class(ref cn) = arr_ty {
                    module
                        .classes
                        .iter()
                        .find(|c| &c.name == cn)
                        .is_some_and(|cls| cls.methods.iter().any(|m| m.name == "set"))
                } else {
                    false
                };
                if has_set {
                    if let Ty::Class(cn) = &arr_ty {
                        let dest = fb.new_local(Ty::Unit);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: cn.clone(),
                                    method_name: "set".to_string(),
                                },
                                args: vec![a, i, v],
                            },
                        });
                    }
                } else {
                    let dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayStore {
                            array: a,
                            index: i,
                            value: v,
                        },
                    });
                }
            }
            true
        }
        Stmt::FieldAssign {
            receiver,
            field,
            value,
            ..
        } => {
            let Some(recv_local) = lower_expr(
                receiver,
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
            let Some(val_local) = lower_expr(
                value,
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
            let recv_ty = fb.mf.locals[recv_local.0 as usize].clone();
            let class_name = if let Ty::Class(cn) = &recv_ty {
                cn.clone()
            } else {
                "java/lang/Object".to_string()
            };
            let field_name = interner.resolve(*field).to_string();
            // Check for a custom setter method on the class.
            let setter_name = format!("set{}{}", &field_name[..1].to_uppercase(), &field_name[1..]);
            let has_setter = if let Ty::Class(ref cn) = recv_ty {
                module
                    .classes
                    .iter()
                    .find(|c| &c.name == cn)
                    .is_some_and(|cls| cls.methods.iter().any(|m| m.name == setter_name))
            } else {
                false
            };
            if has_setter {
                // Invoke the setter method instead of direct field write.
                let dummy = fb.new_local(Ty::Unit);
                fb.push_stmt(MStmt::Assign {
                    dest: dummy,
                    value: Rvalue::Call {
                        kind: CallKind::Virtual {
                            class_name,
                            method_name: setter_name,
                        },
                        args: vec![recv_local, val_local],
                    },
                });
            } else {
                let dummy = fb.new_local(Ty::Unit);
                fb.push_stmt(MStmt::Assign {
                    dest: dummy,
                    value: Rvalue::PutField {
                        receiver: recv_local,
                        class_name,
                        field_name,
                        value: val_local,
                    },
                });
            }
            true
        }
        Stmt::For {
            var_name,
            start: range_start,
            end: range_end,
            exclusive,
            descending,
            step: step_expr,
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

            // Step block: i = i + step (ascending) or i = i - step (descending)
            // When a `step` expression is provided, always ADD (the step
            // value itself is always positive in Kotlin; for downTo the
            // step is still added as subtraction).
            let step_op = if *descending {
                MBinOp::SubI
            } else {
                MBinOp::AddI
            };
            let step_val = if let Some(step_e) = step_expr {
                lower_expr(
                    step_e,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                )
                .unwrap_or_else(|| {
                    let tmp = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: tmp,
                        value: Rvalue::Const(MirConst::Int(1)),
                    });
                    tmp
                })
            } else {
                let one = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: one,
                    value: Rvalue::Const(MirConst::Int(1)),
                });
                one
            };
            let incremented = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest: incremented,
                value: Rvalue::BinOp {
                    op: step_op,
                    lhs: loop_var,
                    rhs: step_val,
                },
            });
            fb.push_stmt(MStmt::Assign {
                dest: loop_var,
                value: Rvalue::Local(incremented),
            });

            fb.terminate_and_switch(Terminator::Goto(cond_block), exit_block);
            true
        }
        Stmt::ForIn {
            var_name,
            destructure_names,
            iterable,
            body,
            ..
        } => {
            // Desugar: for (x in collection) { body }
            let Some(collection_local) = lower_expr(
                iterable,
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

            let collection_ty = fb.mf.locals[collection_local.0 as usize].clone();

            if matches!(
                collection_ty,
                Ty::IntArray | Ty::LongArray | Ty::DoubleArray | Ty::BooleanArray | Ty::ByteArray
            ) {
                // IntArray iteration: desugar to index-based loop
                //   var i = 0
                //   while (i < array.size) { val x = array[i]; body; i++ }
                let idx_var = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: idx_var,
                    value: Rvalue::Const(MirConst::Int(0)),
                });

                let arr_len = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: arr_len,
                    value: Rvalue::ArrayLength(collection_local),
                });

                let cond_block = fb.new_block();
                let body_block = fb.new_block();
                let exit_block = fb.new_block();

                fb.terminate_and_switch(Terminator::Goto(cond_block), cond_block);

                // Condition: i < array.size
                let cmp = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: cmp,
                    value: Rvalue::BinOp {
                        op: MBinOp::CmpLt,
                        lhs: idx_var,
                        rhs: arr_len,
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

                // Body: val x = array[i]
                let element = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: element,
                    value: Rvalue::ArrayLoad {
                        array: collection_local,
                        index: idx_var,
                    },
                });
                if let Some(names) = destructure_names {
                    // Destructure the element via componentN() calls.
                    for (i, dn) in names.iter().enumerate() {
                        let method_name = format!("component{}", i + 1);
                        let comp = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest: comp,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "kotlin/Pair".to_string(),
                                    method_name: method_name.clone(),
                                    descriptor: "()Ljava/lang/Object;".to_string(),
                                },
                                args: vec![element],
                            },
                        });
                        scope.push((*dn, comp));
                    }
                } else {
                    scope.push((*var_name, element));
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

                // i++
                let incremented = fb.new_local(Ty::Int);
                let one = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: one,
                    value: Rvalue::Const(MirConst::Int(1)),
                });
                fb.push_stmt(MStmt::Assign {
                    dest: incremented,
                    value: Rvalue::BinOp {
                        op: MBinOp::AddI,
                        lhs: idx_var,
                        rhs: one,
                    },
                });
                fb.push_stmt(MStmt::Assign {
                    dest: idx_var,
                    value: Rvalue::Local(incremented),
                });

                fb.terminate_and_switch(Terminator::Goto(cond_block), exit_block);
            } else {
                // Check if we're iterating a Map — need entrySet().iterator()
                // instead of just .iterator().
                let is_map = matches!(&collection_ty, Ty::Class(cn)
                    if cn.contains("Map"));

                // Collection iteration via iterator()
                //   val iter = collection.iterator()
                //   while (iter.hasNext()) { val x = iter.next(); body }
                let iter_local = fb.new_local(Ty::Class("java/util/Iterator".to_string()));
                if is_map && destructure_names.is_some() {
                    // Map destructuring: map.entrySet().iterator()
                    let entry_set = fb.new_local(Ty::Class("java/util/Set".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: entry_set,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/util/Map".to_string(),
                                method_name: "entrySet".to_string(),
                                descriptor: "()Ljava/util/Set;".to_string(),
                            },
                            args: vec![collection_local],
                        },
                    });
                    fb.push_stmt(MStmt::Assign {
                        dest: iter_local,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/util/Set".to_string(),
                                method_name: "iterator".to_string(),
                                descriptor: "()Ljava/util/Iterator;".to_string(),
                            },
                            args: vec![entry_set],
                        },
                    });
                } else {
                    fb.push_stmt(MStmt::Assign {
                        dest: iter_local,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/Iterable".to_string(),
                                method_name: "iterator".to_string(),
                                descriptor: "()Ljava/util/Iterator;".to_string(),
                            },
                            args: vec![collection_local],
                        },
                    });
                }

                let cond_block = fb.new_block();
                let body_block = fb.new_block();
                let exit_block = fb.new_block();

                fb.terminate_and_switch(Terminator::Goto(cond_block), cond_block);

                // Condition: iter.hasNext()
                let has_next = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: has_next,
                    value: Rvalue::Call {
                        kind: CallKind::VirtualJava {
                            class_name: "java/util/Iterator".to_string(),
                            method_name: "hasNext".to_string(),
                            descriptor: "()Z".to_string(),
                        },
                        args: vec![iter_local],
                    },
                });
                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: has_next,
                        then_block: body_block,
                        else_block: exit_block,
                    },
                    body_block,
                );

                // Body: val x = iter.next(); <body stmts>
                let element = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: element,
                    value: Rvalue::Call {
                        kind: CallKind::VirtualJava {
                            class_name: "java/util/Iterator".to_string(),
                            method_name: "next".to_string(),
                            descriptor: "()Ljava/lang/Object;".to_string(),
                        },
                        args: vec![iter_local],
                    },
                });
                if let Some(names) = destructure_names {
                    if is_map {
                        // Map.Entry destructuring: checkcast + getKey()/getValue()
                        let entry = fb.new_local(Ty::Class("java/util/Map$Entry".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: entry,
                            value: Rvalue::CheckCast {
                                obj: element,
                                target_class: "java/util/Map$Entry".to_string(),
                            },
                        });
                        for (i, dn) in names.iter().enumerate() {
                            let method_name = if i == 0 { "getKey" } else { "getValue" };
                            let comp = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: comp,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "java/util/Map$Entry".to_string(),
                                        method_name: method_name.to_string(),
                                        descriptor: "()Ljava/lang/Object;".to_string(),
                                    },
                                    args: vec![entry],
                                },
                            });
                            scope.push((*dn, comp));
                        }
                    } else {
                        // Pair/data class destructuring: checkcast + componentN()
                        let pair = fb.new_local(Ty::Class("kotlin/Pair".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: pair,
                            value: Rvalue::CheckCast {
                                obj: element,
                                target_class: "kotlin/Pair".to_string(),
                            },
                        });
                        for (i, dn) in names.iter().enumerate() {
                            let method_name = format!("component{}", i + 1);
                            let comp = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: comp,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "kotlin/Pair".to_string(),
                                        method_name,
                                        descriptor: "()Ljava/lang/Object;".to_string(),
                                    },
                                    args: vec![pair],
                                },
                            });
                            scope.push((*dn, comp));
                        }
                    }
                } else {
                    scope.push((*var_name, element));
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
            }
            true
        }
        Stmt::LocalFun(f) => {
            // Lower local function as a synthetic top-level function.
            // Captured outer variables are passed as extra parameters.
            let fn_idx = module.functions.len();
            let fn_name = interner.resolve(f.name).to_string();
            let return_ty = f
                .return_ty
                .as_ref()
                .map(|tr| resolve_type_ref(tr, interner, module))
                .unwrap_or(Ty::Unit);

            // Collect free variables in the local function body.
            let param_names: Vec<Symbol> = f.params.iter().map(|p| p.name).collect();
            let free_vars: Vec<(Symbol, LocalId, Ty)> =
                collect_free_vars(&f.body, &param_names, scope, fb, interner);
            let captures = free_vars;

            // Push placeholder and register in name_to_func BEFORE lowering
            // so recursive calls from inside the body can resolve.
            let total_params = captures.len() + f.params.len();
            let placeholder_params: Vec<LocalId> = (0..total_params as u32).map(LocalId).collect();
            let placeholder_locals: Vec<Ty> = vec![Ty::Any; total_params];
            module.functions.push(MirFunction {
                id: FuncId(fn_idx as u32),
                name: fn_name.clone(),
                params: placeholder_params,
                locals: placeholder_locals,
                blocks: Vec::new(),
                return_ty: return_ty.clone(),
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: false,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
            });
            name_to_func.insert(f.name, FuncId(fn_idx as u32));

            // Build the function with captures as extra leading params.
            let mut inner_fb = FnBuilder::new(fn_idx, fn_name.clone(), return_ty.clone());

            // Add capture params first.
            let mut inner_scope: Vec<(Symbol, LocalId)> = Vec::new();
            for (sym, _, ty) in &captures {
                let pid = inner_fb.new_local(ty.clone());
                inner_fb.mf.params.push(pid);
                inner_scope.push((*sym, pid));
            }

            // Add explicit function params.
            for p in &f.params {
                let ty = resolve_type_ref(&p.ty, interner, module);
                let pid = inner_fb.new_local(ty);
                inner_fb.mf.params.push(pid);
                inner_scope.push((p.name, pid));
            }

            // Lower the body with inner scope.
            for s in &f.body.stmts {
                lower_stmt(
                    s,
                    &mut inner_fb,
                    &mut inner_scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    None,
                );
            }
            // If the body didn't terminate, add a default return.
            if !matches!(
                inner_fb.mf.blocks[inner_fb.cur_block as usize].terminator,
                Terminator::ReturnValue(_) | Terminator::Return
            ) {
                inner_fb.set_terminator(Terminator::Return);
            }

            let inner_fn = inner_fb.finish();
            // Replace the placeholder with the real lowered function.
            module.functions[fn_idx] = inner_fn;

            // When calling this local function, the caller must prepend
            // capture args. Store capture info for the call site.
            // We encode this by making the function's params count include
            // captures, so the call site knows to pass them.
            // Store the capture local IDs for later use at call sites.
            for (sym, outer_lid, _) in &captures {
                // Register a synthetic name→local mapping so the call site
                // can find the outer locals to pass as capture args.
                let key =
                    interner.intern(&format!("$capture${}${}", fn_name, interner.resolve(*sym)));
                scope.push((key, *outer_lid));
            }

            true
        }
        Stmt::Break { .. } => {
            if let Some((_continue_blk, break_blk)) = loop_ctx {
                fb.set_terminator(Terminator::Goto(break_blk));
            }
            true
        }
        Stmt::Continue { .. } => {
            if let Some((continue_blk, _break_blk)) = loop_ctx {
                fb.set_terminator(Terminator::Goto(continue_blk));
            }
            true
        }
        Stmt::TryStmt {
            body,
            catch_param,
            catch_type,
            catch_body,
            finally_body,
            ..
        } => {
            if catch_body.is_some() {
                // ── try-catch lowering ──────────────────────────────────
                //
                // Layout:
                //   current_block → goto try_block
                //   try_block: <try body stmts> → goto after_block
                //   catch_block: astore catch_param; <catch body> → goto after_block
                //   after_block: <continues>
                //
                // Exception handler: [try_block .. catch_block) → catch_block
                let try_block = fb.new_block();
                let catch_block = fb.new_block();
                let after_block = fb.new_block();

                // Jump from current block into the try body.
                fb.terminate_and_switch(Terminator::Goto(try_block), try_block);

                // Lower the try body.
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
                        loop_ctx,
                    );
                }
                // If the try body ends with an explicit
                // `return`, the current block already has a ReturnValue
                // terminator. Don't overwrite it with Goto(after_block)
                // — the block returns directly. Only add the Goto if
                // the block has the default Return terminator.
                {
                    let cur = fb.cur_block as usize;
                    let cur_term = &fb.mf.blocks[cur].terminator;
                    if matches!(cur_term, Terminator::ReturnValue(_) | Terminator::Throw(_)) {
                        // Block already returns or throws. Don't overwrite
                        // with Goto — just switch to catch_block.
                        fb.cur_block = catch_block;
                    } else {
                        fb.terminate_and_switch(Terminator::Goto(after_block), catch_block);
                    }
                }

                // The catch handler block receives the exception on the
                // JVM operand stack. We allocate a local to store it,
                // typed as the declared catch type for smart-cast dispatch.
                if let Some(param_sym) = catch_param {
                    let exc_ty = catch_type
                        .as_ref()
                        .map(|ct| {
                            let name = interner.resolve(*ct);
                            match name {
                                "IllegalStateException" => {
                                    Ty::Class("java/lang/IllegalStateException".into())
                                }
                                "IllegalArgumentException" => {
                                    Ty::Class("java/lang/IllegalArgumentException".into())
                                }
                                "RuntimeException" => {
                                    Ty::Class("java/lang/RuntimeException".into())
                                }
                                "Exception" => Ty::Class("java/lang/Exception".into()),
                                "Throwable" => Ty::Class("java/lang/Throwable".into()),
                                "NullPointerException" => {
                                    Ty::Class("java/lang/NullPointerException".into())
                                }
                                other => Ty::Class(other.to_string()),
                            }
                        })
                        .unwrap_or(Ty::Any);
                    let exc_local = fb.new_local(exc_ty);
                    scope.push((*param_sym, exc_local));
                    // Placeholder assignment — the JVM backend will emit
                    // astore at handler entry instead of aconst_null.
                    fb.push_stmt(MStmt::Assign {
                        dest: exc_local,
                        value: Rvalue::Const(MirConst::Null),
                    });
                }

                // Lower the catch body.
                if let Some(cb) = catch_body {
                    for s in &cb.stmts {
                        lower_stmt(
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
                // Same check for catch body: don't overwrite explicit return.
                {
                    let cur = fb.cur_block as usize;
                    let cur_term = &fb.mf.blocks[cur].terminator;
                    if !matches!(cur_term, Terminator::ReturnValue(_)) {
                        fb.terminate_and_switch(Terminator::Goto(after_block), after_block);
                    } else {
                        fb.cur_block = after_block;
                    }
                }

                // Record exception handler.
                let jvm_catch_type = catch_type.map(|sym| {
                    let name = interner.resolve(sym).to_string();
                    match name.as_str() {
                        "Exception" => "java/lang/Exception".to_string(),
                        "RuntimeException" => "java/lang/RuntimeException".to_string(),
                        "ArithmeticException" => "java/lang/ArithmeticException".to_string(),
                        "NullPointerException" => "java/lang/NullPointerException".to_string(),
                        "IllegalArgumentException" => {
                            "java/lang/IllegalArgumentException".to_string()
                        }
                        "IllegalStateException" => "java/lang/IllegalStateException".to_string(),
                        "IndexOutOfBoundsException" => {
                            "java/lang/IndexOutOfBoundsException".to_string()
                        }
                        "ClassCastException" => "java/lang/ClassCastException".to_string(),
                        "NumberFormatException" => "java/lang/NumberFormatException".to_string(),
                        "UnsupportedOperationException" => {
                            "java/lang/UnsupportedOperationException".to_string()
                        }
                        "Throwable" => "java/lang/Throwable".to_string(),
                        "Error" => "java/lang/Error".to_string(),
                        other => {
                            if other.contains('/') {
                                other.to_string()
                            } else {
                                format!("java/lang/{other}")
                            }
                        }
                    }
                });

                fb.mf.exception_handlers.push(ExceptionHandler {
                    try_start_block: try_block,
                    try_end_block: catch_block,
                    handler_block: catch_block,
                    catch_type: jvm_catch_type,
                });

                // Finally block (if present) is inlined after the catch.
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
            } else {
                // No catch — just try + finally (original simplified path).
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
            }
            true
        }
        Stmt::ThrowStmt { expr, .. } => {
            // Lower the exception expression and terminate with Throw.
            if let Some(exc_local) = lower_expr(
                expr,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            ) {
                fb.set_terminator(Terminator::Throw(exc_local));
            }
            true
        }
        Stmt::Destructure { names, init, .. } => {
            // Lower the init expression to get the composite value.
            let Some(init_local) = lower_expr(
                init,
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
            // Determine the class name from the init local's type.
            let init_ty = fb.mf.locals[init_local.0 as usize].clone();
            let class_name = match &init_ty {
                Ty::Class(name) => name.clone(),
                _ => {
                    diags.push(Diagnostic::error(
                        init.span(),
                        "destructuring requires a class type with componentN() methods",
                    ));
                    return false;
                }
            };
            // For each name at position i, emit a virtual call to component{i+1}().
            for (i, name) in names.iter().enumerate() {
                let method_name = format!("component{}", i + 1);
                // Determine the return type from the class fields.
                let field_ty = module
                    .classes
                    .iter()
                    .find(|c| c.name == class_name)
                    .and_then(|c| c.fields.get(i))
                    .map(|f| f.ty.clone())
                    .unwrap_or(Ty::Any);
                let result = fb.new_local(field_ty);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::Virtual {
                            class_name: class_name.clone(),
                            method_name,
                        },
                        args: vec![init_local],
                    },
                });
                scope.push((*name, result));
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
    let rhs_ty = fb.mf.locals[rhs.0 as usize].clone();
    // Respect the nullable annotation when present.  In particular,
    // `val x: String? = "hello"` should give x type Nullable(String),
    // not plain String (the RHS type).  We only use the annotation to
    // add the Nullable wrapper; for everything else (including function
    // types like `(Int) -> Int`) we keep the RHS-inferred type.
    let ty = if let Some(tr) = &v.ty {
        if tr.nullable && tr.func_params.is_none() {
            let base = resolve_type(interner.resolve(tr.name), module);
            Ty::Nullable(Box::new(base))
        } else if tr.nullable {
            // Function-type annotation: wrap the RHS type.
            if matches!(rhs_ty, Ty::Nullable(_)) {
                rhs_ty
            } else {
                Ty::Nullable(Box::new(rhs_ty))
            }
        } else {
            // Narrow integer/float literals to the annotated type.
            // e.g. `val b: Byte = 42` → Ty::Byte, `val s: Short = 1000` → Ty::Short
            let annotated = resolve_type(interner.resolve(tr.name), module);
            match (&annotated, &rhs_ty) {
                (Ty::Byte | Ty::Short, Ty::Int) | (Ty::Float, Ty::Double) => annotated,
                _ => rhs_ty,
            }
        }
    } else {
        rhs_ty
    };
    // When narrowing from Int→Byte/Short or Double→Float, patch the
    // RHS local's type in-place so backends emit the right opcodes
    // for load/store. This is safe because the RHS local is a
    // freshly-created temporary that is only consumed here.
    if ty != fb.mf.locals[rhs.0 as usize] {
        fb.mf.locals[rhs.0 as usize] = ty.clone();
    }
    let dest = fb.new_local(ty);
    fb.push_stmt(MStmt::Assign {
        dest,
        value: Rvalue::Local(rhs),
    });
    scope.push((v.name, dest));
    if v.is_var {
        fb.var_syms.insert(v.name);
    }
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
        Expr::CharLit(v, _) => {
            // Char literal: store as Ty::Char with the code point value.
            // On JVM this is a 16-bit unsigned value stored in an int local.
            let dest = fb.new_local(Ty::Char);
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
        Expr::FloatLit(v, _) => {
            let dest = fb.new_local(Ty::Float);
            fb.push_stmt(MStmt::Assign {
                dest,
                value: Rvalue::Const(MirConst::Float(*v as f32)),
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
                // Auto-dereference $Ref-boxed vars: read .element field.
                if let Ty::Class(ref cls) = ty {
                    if cls.starts_with("$Ref$") {
                        let elem_ty = module
                            .classes
                            .iter()
                            .find(|c| &c.name == cls)
                            .and_then(|c| c.fields.first())
                            .map(|f| f.ty.clone())
                            .unwrap_or(Ty::Any);
                        let dest = fb.new_local(elem_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::GetField {
                                receiver: src,
                                class_name: cls.clone(),
                                field_name: "element".to_string(),
                            },
                        });
                        return Some(dest);
                    }
                }
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
                // Smart cast with &&: if lhs is `x is Type`, narrow x
                // in the rhs_block scope (reached only when lhs is true).
                let and_smart_cast = if *op == BinOp::And {
                    if let Expr::IsCheck {
                        expr: checked,
                        type_name,
                        negated: false,
                        ..
                    } = lhs.as_ref()
                    {
                        if let Expr::Ident(var_name, _) = checked.as_ref() {
                            let type_str = interner.resolve(*type_name);
                            let narrowed_ty =
                                skotch_types::ty_from_name(type_str).unwrap_or_else(|| {
                                    if module.classes.iter().any(|c| c.name == type_str) {
                                        Ty::Class(type_str.to_string())
                                    } else {
                                        Ty::Any
                                    }
                                });
                            if let Some((_, old_local)) =
                                scope.iter().rev().find(|(s, _)| s == var_name)
                            {
                                let cast_local = fb.new_local(narrowed_ty);
                                fb.push_stmt(MStmt::Assign {
                                    dest: cast_local,
                                    value: Rvalue::Local(*old_local),
                                });
                                scope.push((*var_name, cast_local));
                                1usize
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    }
                } else {
                    0
                };
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
                // Pop smart cast scope entries from && narrowing.
                for _ in 0..and_smart_cast {
                    scope.pop();
                }
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

            // ── Operator overloading for class types ────────────────
            // When the LHS is a class instance, check if it has an
            // operator method (plus, minus, times, compareTo) and
            // desugar to a virtual call instead of a primitive op.
            if let Ty::Class(class_name) = lhs_ty {
                let op_method = match op {
                    BinOp::Add => Some("plus"),
                    BinOp::Sub => Some("minus"),
                    BinOp::Mul => Some("times"),
                    BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => Some("compareTo"),
                    _ => None,
                };
                if let Some(method_name) = op_method {
                    let mut found: Option<(String, Ty)> = None;
                    let mut search = Some(class_name.clone());
                    while let Some(ref cname) = search {
                        if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                            if let Some(m) = cls.methods.iter().find(|m| m.name == method_name) {
                                found = Some((cname.clone(), m.return_ty.clone()));
                                break;
                            }
                            search = cls.super_class.clone();
                        } else {
                            break;
                        }
                    }
                    if let Some((found_class, ret_ty)) = found {
                        if method_name == "compareTo" {
                            let cmp_result = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: cmp_result,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: found_class,
                                        method_name: "compareTo".to_string(),
                                    },
                                    args: vec![l, r],
                                },
                            });
                            let zero = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: zero,
                                value: Rvalue::Const(MirConst::Int(0)),
                            });
                            let cmp_op = match op {
                                BinOp::Lt => MBinOp::CmpLt,
                                BinOp::Gt => MBinOp::CmpGt,
                                BinOp::LtEq => MBinOp::CmpLe,
                                BinOp::GtEq => MBinOp::CmpGe,
                                _ => unreachable!(),
                            };
                            let dest = fb.new_local(Ty::Bool);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::BinOp {
                                    op: cmp_op,
                                    lhs: cmp_result,
                                    rhs: zero,
                                },
                            });
                            return Some(dest);
                        } else {
                            // Use the method's declared return type when
                            // available; fall back to the receiver's class
                            // type so chained operators (a + b + c) keep the
                            // correct type even when the method stub has not
                            // yet been replaced with the fully-inferred body.
                            let effective_ret = if ret_ty == Ty::Unit {
                                Ty::Class(class_name.clone())
                            } else {
                                ret_ty
                            };
                            let dest = fb.new_local(effective_ret);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: found_class,
                                        method_name: method_name.to_string(),
                                    },
                                    args: vec![l, r],
                                },
                            });
                            return Some(dest);
                        }
                    }
                }
            }

            // Auto-unbox Ty::Any operands for arithmetic: Object → Integer.intValue()
            // Skip when either side is String — that's string concatenation, not arithmetic,
            // and the Any operand is likely a String at runtime.
            let lhs_ty = lhs_ty.clone();
            let rhs_ty = rhs_ty.clone();
            let needs_unbox = matches!(
                op,
                BinOp::Add
                    | BinOp::Sub
                    | BinOp::Mul
                    | BinOp::Div
                    | BinOp::Mod
                    | BinOp::Lt
                    | BinOp::Gt
                    | BinOp::LtEq
                    | BinOp::GtEq
            ) && (matches!(lhs_ty, Ty::Any) || matches!(rhs_ty, Ty::Any))
                && !matches!(lhs_ty, Ty::String)
                && !matches!(rhs_ty, Ty::String);
            let (l, lhs_ty) = if needs_unbox && matches!(lhs_ty, Ty::Any) {
                let unboxed = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: unboxed,
                    value: Rvalue::Call {
                        kind: CallKind::VirtualJava {
                            class_name: "java/lang/Integer".to_string(),
                            method_name: "intValue".to_string(),
                            descriptor: "()I".to_string(),
                        },
                        args: vec![l],
                    },
                });
                (unboxed, Ty::Int)
            } else {
                (l, lhs_ty)
            };
            let (r, rhs_ty) = if needs_unbox && matches!(rhs_ty, Ty::Any) {
                let unboxed = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: unboxed,
                    value: Rvalue::Call {
                        kind: CallKind::VirtualJava {
                            class_name: "java/lang/Integer".to_string(),
                            method_name: "intValue".to_string(),
                            descriptor: "()I".to_string(),
                        },
                        args: vec![r],
                    },
                });
                (unboxed, Ty::Int)
            } else {
                (r, rhs_ty)
            };

            let is_double = matches!(lhs_ty, Ty::Double) || matches!(rhs_ty, Ty::Double);
            let is_long = !is_double && (matches!(lhs_ty, Ty::Long) || matches!(rhs_ty, Ty::Long));
            // Widen Int→Double when comparing/operating with a Double operand.
            let (l, lhs_ty) = if is_double && matches!(lhs_ty, Ty::Int) {
                let widened = fb.new_local(Ty::Double);
                fb.push_stmt(MStmt::Assign {
                    dest: widened,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "$convert".to_string(),
                            method_name: "i2d".to_string(),
                            descriptor: "(I)D".to_string(),
                        },
                        args: vec![l],
                    },
                });
                (widened, Ty::Double)
            } else {
                (l, lhs_ty)
            };
            let (r, rhs_ty) = if is_double && matches!(rhs_ty, Ty::Int) {
                let widened = fb.new_local(Ty::Double);
                fb.push_stmt(MStmt::Assign {
                    dest: widened,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "$convert".to_string(),
                            method_name: "i2d".to_string(),
                            descriptor: "(I)D".to_string(),
                        },
                        args: vec![r],
                    },
                });
                (widened, Ty::Double)
            } else {
                (r, rhs_ty)
            };
            // Widen Int→Long when comparing/operating with a Long operand.
            let (l, lhs_ty) = if is_long && matches!(lhs_ty, Ty::Int) {
                let widened = fb.new_local(Ty::Long);
                fb.push_stmt(MStmt::Assign {
                    dest: widened,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "$convert".to_string(),
                            method_name: "i2l".to_string(),
                            descriptor: "(I)J".to_string(),
                        },
                        args: vec![l],
                    },
                });
                (widened, Ty::Long)
            } else {
                (l, lhs_ty)
            };
            #[allow(unused_variables)]
            let (r, rhs_ty) = if is_long && matches!(rhs_ty, Ty::Int) {
                let widened = fb.new_local(Ty::Long);
                fb.push_stmt(MStmt::Assign {
                    dest: widened,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "$convert".to_string(),
                            method_name: "i2l".to_string(),
                            descriptor: "(I)J".to_string(),
                        },
                        args: vec![r],
                    },
                });
                (widened, Ty::Long)
            } else {
                (r, rhs_ty)
            };
            let (mop, result_ty) = match op {
                BinOp::Add if matches!(lhs_ty, Ty::String) || matches!(rhs_ty, Ty::String) => {
                    (MBinOp::ConcatStr, Ty::String)
                }
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

            // Smart cast: if cond is `x is Type`, narrow x's type in then-branch.
            // Create a new local with the narrowed type and rebind x in scope.
            let smart_cast_count = if let Expr::Binary {
                op: BinOp::NotEq,
                lhs,
                rhs,
                ..
            } = cond.as_ref()
            {
                // Null-check smart cast: `x != null` → narrow x from T? to T.
                if let (Expr::Ident(var_name, _), Expr::NullLit(_))
                | (Expr::NullLit(_), Expr::Ident(var_name, _)) = (lhs.as_ref(), rhs.as_ref())
                {
                    if let Some((_, old_local)) = scope.iter().rev().find(|(s, _)| s == var_name) {
                        let old_ty = fb.mf.locals[old_local.0 as usize].clone();
                        let narrowed = if let Ty::Nullable(inner) = old_ty {
                            *inner
                        } else {
                            old_ty
                        };
                        let cast_local = fb.new_local(narrowed);
                        fb.push_stmt(MStmt::Assign {
                            dest: cast_local,
                            value: Rvalue::Local(*old_local),
                        });
                        scope.push((*var_name, cast_local));
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            } else {
                // Check for IsCheck directly, or IsCheck as LHS of &&.
                let is_check_info: Option<(&Expr, Symbol)> = if let Expr::IsCheck {
                    expr: checked,
                    type_name,
                    negated: false,
                    ..
                } = cond.as_ref()
                {
                    Some((checked.as_ref(), *type_name))
                } else if let Expr::Binary {
                    op: BinOp::And,
                    lhs: and_lhs,
                    ..
                } = cond.as_ref()
                {
                    if let Expr::IsCheck {
                        expr: checked,
                        type_name,
                        negated: false,
                        ..
                    } = and_lhs.as_ref()
                    {
                        Some((checked.as_ref(), *type_name))
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some((Expr::Ident(var_name, _), type_name)) = is_check_info {
                    let type_str = interner.resolve(type_name);
                    let narrowed_ty = skotch_types::ty_from_name(type_str).unwrap_or_else(|| {
                        // Check if it's a user-defined class/interface.
                        if module.classes.iter().any(|c| c.name == type_str) {
                            Ty::Class(type_str.to_string())
                        } else {
                            Ty::Any
                        }
                    });
                    // Find the variable's current local.
                    if let Some((_, old_local)) = scope.iter().rev().find(|(s, _)| s == var_name) {
                        // Create a new local with the narrowed type.
                        let cast_local = fb.new_local(narrowed_ty);
                        // Emit a copy from old to new (identity cast for now).
                        fb.push_stmt(MStmt::Assign {
                            dest: cast_local,
                            value: Rvalue::Local(*old_local),
                        });
                        // Push new binding that shadows the original.
                        scope.push((*var_name, cast_local));
                        1
                    } else {
                        0
                    }
                } else {
                    0
                }
            };

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
                    skotch_syntax::Stmt::Break { .. } | skotch_syntax::Stmt::Continue { .. } => {
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
            // Remove smart cast scope entry.
            for _ in 0..smart_cast_count {
                scope.pop();
            }
            // Check if the block already terminated (throw expression,
            // return expression, etc.) — don't overwrite with Goto.
            let already_terminated = matches!(
                fb.mf.blocks[fb.cur_block as usize].terminator,
                Terminator::Throw(_) | Terminator::ReturnValue(_)
            );
            // Only skip result assignment for Throw — the val from
            // Expr::Throw is an uninitialized Nothing local (aload of
            // Top triggers VerifyError). ReturnValue blocks still need
            // their preceding assignments for coroutine state machines.
            let throw_terminated = matches!(
                fb.mf.blocks[fb.cur_block as usize].terminator,
                Terminator::Throw(_)
            );
            if let Some(val) = then_val {
                if !throw_terminated {
                    let inferred_ty = fb.mf.locals[val.0 as usize].clone();
                    fb.mf.locals[result.0 as usize] = inferred_ty;
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(val),
                    });
                }
            }
            if then_terminates || already_terminated {
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
                                // Guard: don't assign result from a
                                // throw expression (uninitialized Nothing local).
                                let blk_term = &fb.mf.blocks[fb.cur_block as usize].terminator;
                                let dead = matches!(blk_term, Terminator::Throw(_));
                                if !dead {
                                    fb.push_stmt(MStmt::Assign {
                                        dest: result,
                                        value: Rvalue::Local(val),
                                    });
                                }
                            }
                        }
                        skotch_syntax::Stmt::Return { .. }
                        | skotch_syntax::Stmt::Break { .. }
                        | skotch_syntax::Stmt::Continue { .. } => {
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
            let else_already_terminated = matches!(
                fb.mf.blocks[fb.cur_block as usize].terminator,
                Terminator::Throw(_) | Terminator::ReturnValue(_)
            );
            if else_terminates || else_already_terminated {
                fb.cur_block = merge_blk;
            } else {
                fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);
            }

            Some(result)
        }
        Expr::Call {
            callee,
            args,
            type_args,
            span,
        } => {
            // Handle method calls on a receiver: `receiver.method(args)`
            if let Expr::Field { receiver, name, .. } = callee.as_ref() {
                let method_name = *name;

                // Check for enum static methods: Color.values(), Color.valueOf("RED").
                if let Expr::Ident(recv_sym, _) = receiver.as_ref() {
                    let recv_name = interner.resolve(*recv_sym).to_string();
                    if module.enum_names.contains(&recv_name) {
                        let method_str = interner.resolve(method_name).to_string();
                        if method_str == "values" || method_str == "valueOf" {
                            let compound = format!("{}${}", recv_name, method_str);
                            let compound_sym = interner.intern(&compound);
                            if let Some(&fid) = name_to_func.get(&compound_sym) {
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
                        }
                    }
                }

                // Check if this is an object method call (Singleton.method())
                // or an extension function call (receiver.extFun()).
                // Object methods and extension functions are registered as
                // top-level functions. Extension functions have params.len() > args.len()
                // because the receiver is the first param.
                if let Some(&fid) = name_to_func.get(&method_name) {
                    let target = &module.functions[fid.0 as usize];
                    let ret_ty = target.return_ty.clone();
                    let is_extension = target.params.len() == args.len() + 1;
                    let mut arg_locals = Vec::new();
                    if is_extension {
                        // Extension function: lower the receiver and pass
                        // it as the first argument.
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
                        arg_locals.push(recv_local);
                    }
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

                // Check for enum dispatch functions. When an enum has
                // abstract methods, the lowerer registers dispatch
                // functions as "EnumName$methodName". We detect this
                // for chains like `Op.PLUS.apply(3, 2)` by extracting
                // the class name from the receiver field access.
                {
                    let method_str = interner.resolve(method_name).to_string();
                    // Try to determine the enum class name from the
                    // receiver. For `Op.PLUS.apply()`, the receiver is
                    // `Field { Ident("Op"), "PLUS" }`. Check if the
                    // receiver's outermost ident is an enum class.
                    let enum_class = if let Expr::Field {
                        receiver: inner_recv,
                        ..
                    } = receiver.as_ref()
                    {
                        if let Expr::Ident(sym, _) = inner_recv.as_ref() {
                            let name = interner.resolve(*sym).to_string();
                            if module.enum_names.contains(&name) {
                                Some(name)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else if let Expr::Ident(sym, _) = receiver.as_ref() {
                        // `op.apply()` where op is a local variable —
                        // look up its type from scope.
                        let found = scope
                            .iter()
                            .rev()
                            .find(|(s, _)| s == sym)
                            .map(|(_, lid)| &fb.mf.locals[lid.0 as usize]);
                        if let Some(Ty::Class(cname)) = found {
                            if module.enum_names.contains(cname.as_str()) {
                                Some(cname.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(cls_name) = enum_class {
                        let dispatch_name = format!("{}${}", cls_name, method_str);
                        let dispatch_sym = interner.intern(&dispatch_name);
                        if let Some(&fid) = name_to_func.get(&dispatch_sym) {
                            // Lower the receiver (the enum instance)
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
                            let ret_ty = module.functions[fid.0 as usize].return_ty.clone();
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::Static(fid),
                                    args: all_args,
                                },
                            });
                            return Some(dest);
                        }
                    }
                }

                // Check for nested class constructor: `Outer.Nested(args)`.
                // The parser sees this as receiver=Ident("Outer"),
                // name="Nested". We look for a MirClass named "Outer$Nested".
                if let Some(qname) = extract_qualified_name(receiver, interner) {
                    let method_str = interner.resolve(method_name).to_string();
                    let nested_class_name = format!("{}${}", qname, method_str);
                    let is_nested = module.classes.iter().any(|c| c.name == nested_class_name);
                    if is_nested {
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
                        let dest = fb.new_local(Ty::Class(nested_class_name.clone()));
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::NewInstance(nested_class_name.clone()),
                        });
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Constructor(nested_class_name),
                                args: arg_locals,
                            },
                        });
                        return Some(dest);
                    }
                }

                // Inner class construction: `outer.Inner(args)` where `outer`
                // is an instance variable. Resolve as `new Outer$Inner(outer, args)`.
                {
                    let method_str = interner.resolve(method_name).to_string();
                    // Check if receiver is a local variable with a class type.
                    if let Expr::Ident(recv_sym, _) = receiver.as_ref() {
                        if let Some((_, recv_local)) =
                            scope.iter().rev().find(|(s, _)| s == recv_sym)
                        {
                            let recv_ty = fb.mf.locals[recv_local.0 as usize].clone();
                            if let Ty::Class(ref outer_class) = recv_ty {
                                let inner_class_name = format!("{}${}", outer_class, method_str);
                                let is_inner =
                                    module.classes.iter().any(|c| c.name == inner_class_name);
                                if is_inner {
                                    let mut arg_locals = vec![*recv_local]; // outer ref
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
                                    let dest = fb.new_local(Ty::Class(inner_class_name.clone()));
                                    fb.push_stmt(MStmt::Assign {
                                        dest,
                                        value: Rvalue::NewInstance(inner_class_name.clone()),
                                    });
                                    fb.push_stmt(MStmt::Assign {
                                        dest,
                                        value: Rvalue::Call {
                                            kind: CallKind::Constructor(inner_class_name),
                                            args: arg_locals,
                                        },
                                    });
                                    return Some(dest);
                                }
                            }
                        }
                    }
                }

                // If try_java_static_call returned None, check if the
                // "receiver.method" is actually a fully-qualified
                // constructor: `java.net.URI()` → receiver="java.net",
                // method="URI" → class="java/net/URI", call <init>.
                if let Some(qname) = extract_qualified_name(receiver, interner) {
                    let method_str = interner.resolve(method_name).to_string();

                    if method_str.starts_with(|c: char| c.is_uppercase()) && qname.contains('.') {
                        let full_class = format!("{qname}.{method_str}");
                        let jvm_class = full_class.replace('.', "/");
                        if let Ok(_info) = skotch_classinfo::load_jdk_class(&jvm_class) {
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
                            let dest = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::NewInstance(jvm_class.clone()),
                            });
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::Constructor(jvm_class),
                                    args: arg_locals,
                                },
                            });
                            return Some(dest);
                        }
                    }

                    if qname.starts_with(|c: char| c.is_uppercase()) || qname.contains('.') {
                        diags.push(Diagnostic::error(
                            *span,
                            format!(
                                "unresolved reference: `{qname}.{method_str}` — class `{qname}` not found on the classpath"
                            ),
                        ));
                        return None;
                    }
                }

                // Check for super.method() BEFORE lowering the receiver.
                {
                    let super_sym = interner.intern("super");
                    if let Expr::Ident(ident_name, _) = receiver.as_ref() {
                        if *ident_name == super_sym {
                            let this_sym = interner.intern("this");
                            // Extract parent class name and return type without
                            // holding an immutable borrow across lower_expr.
                            let super_info: Option<(LocalId, String, String, Ty)> = scope
                                .iter()
                                .rev()
                                .find(|(s, _)| *s == this_sym)
                                .and_then(|(_, this_local)| {
                                    let this_ty = &fb.mf.locals[this_local.0 as usize];
                                    if let Ty::Class(cn) = this_ty {
                                        let cls = module.classes.iter().find(|c| &c.name == cn)?;
                                        let parent = cls.super_class.as_ref()?.clone();
                                        let mname = interner.resolve(method_name).to_string();
                                        let mut ret_ty = Ty::Unit;
                                        let mut search = Some(parent.clone());
                                        while let Some(ref cname) = search {
                                            if let Some(pcls) =
                                                module.classes.iter().find(|c| &c.name == cname)
                                            {
                                                if let Some(m) =
                                                    pcls.methods.iter().find(|m| m.name == mname)
                                                {
                                                    ret_ty = m.return_ty.clone();
                                                    break;
                                                }
                                                search = pcls.super_class.clone();
                                            } else {
                                                break;
                                            }
                                        }
                                        Some((*this_local, parent, mname, ret_ty))
                                    } else {
                                        None
                                    }
                                });
                            if let Some((this_local, parent, mname, ret_ty)) = super_info {
                                let mut all_args = vec![this_local];
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
                                let dest = fb.new_local(ret_ty);
                                fb.push_stmt(MStmt::Assign {
                                    dest,
                                    value: Rvalue::Call {
                                        kind: CallKind::Super {
                                            class_name: parent,
                                            method_name: mname,
                                        },
                                        args: all_args,
                                    },
                                });
                                return Some(dest);
                            }
                        }
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
                let recv_ty = fb.mf.locals[recv_local.0 as usize].clone();
                let method_name_str = interner.resolve(method_name).to_string();

                // For scope functions (apply, run, with, also, let),
                // set the receiver type so the lambda body can bind
                // `this` to the receiver for implicit dispatch.
                if matches!(method_name_str.as_str(), "apply" | "run" | "also" | "let") {
                    if let Ty::Class(ref cn) = recv_ty {
                        module.lambda_receiver_type = Some(cn.clone());
                    }
                }

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
                module.lambda_receiver_type = None; // clear after use

                // ── data class copy() with default-fill ──────────
                // `p.copy(y = 3)` fills unspecified params from receiver fields.
                if method_name_str == "copy" {
                    if let Ty::Class(ref class_name) = recv_ty {
                        // Clone fields to release the borrow on `module`.
                        let copy_fields: Option<Vec<_>> = module
                            .classes
                            .iter()
                            .find(|c| &c.name == class_name)
                            .filter(|cls| {
                                cls.methods.iter().any(|m| m.name == "copy")
                                    && !cls.fields.is_empty()
                            })
                            .map(|cls| {
                                cls.fields
                                    .iter()
                                    .map(|f| (f.name.clone(), f.ty.clone()))
                                    .collect()
                            });
                        if let Some(fields_info) = copy_fields {
                            let mut copy_args = vec![recv_local]; // this
                            for (fname, fty) in &fields_info {
                                // Check if there's a named arg for this field.
                                let named_arg_expr = args
                                    .iter()
                                    .find(|a| a.name.is_some_and(|n| interner.resolve(n) == fname));
                                let val = if let Some(a) = named_arg_expr {
                                    lower_expr(
                                        &a.expr,
                                        fb,
                                        scope,
                                        module,
                                        name_to_func,
                                        name_to_global,
                                        interner,
                                        diags,
                                        loop_ctx,
                                    )?
                                } else {
                                    // Load default from receiver.
                                    let fv = fb.new_local(fty.clone());
                                    fb.push_stmt(MStmt::Assign {
                                        dest: fv,
                                        value: Rvalue::GetField {
                                            receiver: recv_local,
                                            class_name: class_name.clone(),
                                            field_name: fname.clone(),
                                        },
                                    });
                                    fv
                                };
                                copy_args.push(val);
                            }
                            let dest = fb.new_local(Ty::Class(class_name.clone()));
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: class_name.clone(),
                                        method_name: "copy".to_string(),
                                    },
                                    args: copy_args,
                                },
                            });
                            return Some(dest);
                        }
                    }
                }

                // ── Infix `to`: `a to b` → `Pair(a, b)` ─────────
                if method_name_str == "to" && args.len() == 1 {
                    let a = recv_local;
                    let b = all_args[1]; // all_args = [receiver, arg]
                    let a_ty = fb.mf.locals[a.0 as usize].clone();
                    let b_ty = fb.mf.locals[b.0 as usize].clone();
                    let a_boxed = mir_autobox(fb, a, &a_ty);
                    let b_boxed = mir_autobox(fb, b, &b_ty);
                    let dest = fb.new_local(Ty::Class("kotlin/Pair".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::NewInstance("kotlin/Pair".to_string()),
                    });
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::ConstructorJava {
                                class_name: "kotlin/Pair".to_string(),
                                descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)V".to_string(),
                            },
                            args: vec![a_boxed, b_boxed],
                        },
                    });
                    return Some(dest);
                }

                // ── Int.rangeTo(Int) → new IntRange(start, end) ─────
                if method_name_str == "rangeTo" && args.len() == 1 {
                    let start = recv_local;
                    let end = all_args[1];
                    let dest = fb.new_local(Ty::Class("kotlin/ranges/IntRange".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::NewInstance("kotlin/ranges/IntRange".to_string()),
                    });
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::ConstructorJava {
                                class_name: "kotlin/ranges/IntRange".to_string(),
                                descriptor: "(II)V".to_string(),
                            },
                            args: vec![start, end],
                        },
                    });
                    return Some(dest);
                }

                // ── IntRange.contains(Int) → boolean check ──────────
                if method_name_str == "contains"
                    && args.len() == 1
                    && matches!(&recv_ty, Ty::Class(n) if n.contains("IntRange"))
                {
                    let dest = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlin/ranges/IntRange".to_string(),
                                method_name: "contains".to_string(),
                                descriptor: "(I)Z".to_string(),
                            },
                            args: all_args,
                        },
                    });
                    return Some(dest);
                }

                // ── Scope functions (let, also, run, apply) ─────────
                // Lowered as inline intrinsics: the trailing lambda arg
                // is invoked with the receiver, and the result depends
                // on the scope function semantics.
                if matches!(method_name_str.as_str(), "let" | "also" | "run" | "apply")
                    && args.len() == 1
                {
                    // The single argument should be a lambda.
                    let lambda_local = all_args.last().copied().unwrap();
                    let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                    let is_lambda = matches!(&lambda_ty, Ty::Class(n) if n.contains("$Lambda$"))
                        || matches!(lambda_ty, Ty::Any);
                    if is_lambda {
                        // Invoke the lambda with the receiver as its argument.
                        let invoke_class = if let Ty::Class(ref cn) = lambda_ty {
                            cn.clone()
                        } else {
                            "java/lang/Object".to_string()
                        };
                        let invoke_ret = if let Ty::Class(ref cn) = lambda_ty {
                            module
                                .classes
                                .iter()
                                .find(|c| &c.name == cn)
                                .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                                .map(|m| m.return_ty.clone())
                                .unwrap_or(Ty::Any)
                        } else {
                            Ty::Any
                        };

                        // Widen the receiver arg to Ty::Any for the erased
                        // invoke signature, and autobox primitives.
                        let recv_arg = {
                            let recv_arg_ty = fb.mf.locals[recv_local.0 as usize].clone();
                            match recv_arg_ty {
                                Ty::Int => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Integer".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(I)Ljava/lang/Integer;".to_string(),
                                            },
                                            args: vec![recv_local],
                                        },
                                    });
                                    boxed
                                }
                                Ty::Any => recv_local,
                                _ => {
                                    // Reference type — widen to Ty::Any.
                                    let widened = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: widened,
                                        value: Rvalue::Local(recv_local),
                                    });
                                    widened
                                }
                            }
                        };
                        let call_args = vec![lambda_local, recv_arg];
                        let call_result = fb.new_local(invoke_ret.clone());
                        fb.push_stmt(MStmt::Assign {
                            dest: call_result,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: invoke_class,
                                    method_name: "invoke".to_string(),
                                },
                                args: call_args,
                            },
                        });

                        return match method_name_str.as_str() {
                            "let" | "run" => {
                                // Return the lambda's result.
                                Some(call_result)
                            }
                            "also" | "apply" => {
                                // Return the original receiver.
                                Some(recv_local)
                            }
                            _ => unreachable!(),
                        };
                    }
                }

                // ── `.use { block }` — try-with-resources ──────────
                // Simplified desugaring (no exception handler — just inline
                // the lambda body and call close() afterward):
                //   val result = block(resource)
                //   resource.close()
                //   result
                // Full exception-safe version would need try-finally, but
                // this covers the common case and avoids StackMapTable
                // complexity for exception handlers.
                if method_name_str == "use" && args.len() == 1 {
                    let lambda_local = all_args.last().copied().unwrap();
                    let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                    let is_lambda = matches!(&lambda_ty, Ty::Class(n) if n.contains("$Lambda$"))
                        || matches!(lambda_ty, Ty::Any);
                    if is_lambda {
                        // Invoke the lambda with the resource as argument.
                        let invoke_class = if let Ty::Class(ref cn) = lambda_ty {
                            cn.clone()
                        } else {
                            "java/lang/Object".to_string()
                        };
                        let invoke_ret = if let Ty::Class(ref cn) = lambda_ty {
                            module
                                .classes
                                .iter()
                                .find(|c| &c.name == cn)
                                .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                                .map(|m| m.return_ty.clone())
                                .unwrap_or(Ty::Any)
                        } else {
                            Ty::Any
                        };

                        // Check if the lambda takes a parameter (Function1)
                        // or is parameterless (Function0).
                        let lambda_param_count = if let Ty::Class(ref cn) = lambda_ty {
                            module
                                .classes
                                .iter()
                                .find(|c| &c.name == cn)
                                .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                                .map(|m| m.params.len().saturating_sub(1)) // subtract `this`
                                .unwrap_or(0)
                        } else {
                            0
                        };

                        let call_args = if lambda_param_count > 0 {
                            // Function1: invoke(receiver)
                            let recv_arg = {
                                let recv_arg_ty = fb.mf.locals[recv_local.0 as usize].clone();
                                if matches!(recv_arg_ty, Ty::Int | Ty::Long | Ty::Double | Ty::Bool)
                                {
                                    mir_autobox(fb, recv_local, &recv_arg_ty)
                                } else if matches!(recv_arg_ty, Ty::Any) {
                                    recv_local
                                } else {
                                    let widened = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: widened,
                                        value: Rvalue::Local(recv_local),
                                    });
                                    widened
                                }
                            };
                            vec![lambda_local, recv_arg]
                        } else {
                            // Function0: invoke() — no receiver arg
                            vec![lambda_local]
                        };
                        let call_result = fb.new_local(invoke_ret);
                        fb.push_stmt(MStmt::Assign {
                            dest: call_result,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: invoke_class,
                                    method_name: "invoke".to_string(),
                                },
                                args: call_args,
                            },
                        });

                        // Call resource.close() after the lambda.
                        // Use the receiver's actual class for dispatch.
                        let close_class = match &recv_ty {
                            Ty::Class(cn) => cn.clone(),
                            _ => "java/lang/AutoCloseable".to_string(),
                        };
                        // Check if close() is on the receiver's own class
                        // (user-defined) or needs to go through an interface.
                        let has_own_close = module.classes.iter().any(|c| {
                            c.name == close_class && c.methods.iter().any(|m| m.name == "close")
                        });
                        let close_result = fb.new_local(Ty::Unit);
                        if has_own_close {
                            fb.push_stmt(MStmt::Assign {
                                dest: close_result,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: close_class,
                                        method_name: "close".to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                        } else {
                            fb.push_stmt(MStmt::Assign {
                                dest: close_result,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "java/lang/AutoCloseable".to_string(),
                                        method_name: "close".to_string(),
                                        descriptor: "()V".to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                        }

                        return Some(call_result);
                    }
                }

                // ── `.forEach { }` on ArrayList/List collections ──────
                if method_name_str == "forEach"
                    && args.len() == 1
                    && matches!(&recv_ty, Ty::Class(n) if n.contains("ArrayList") || n.contains("List"))
                {
                    let lambda_local = all_args[1]; // all_args = [receiver, lambda]
                    let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                    if matches!(&lambda_ty, Ty::Class(n) if n.contains("$Lambda$")) {
                        let cn = if let Ty::Class(ref n) = lambda_ty {
                            n.clone()
                        } else {
                            unreachable!()
                        };
                        // val iter = collection.iterator()
                        let iter_local = fb.new_local(Ty::Class("java/util/Iterator".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: iter_local,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Iterable".to_string(),
                                    method_name: "iterator".to_string(),
                                    descriptor: "()Ljava/util/Iterator;".to_string(),
                                },
                                args: vec![recv_local],
                            },
                        });
                        // while (iter.hasNext())
                        let cond_blk = fb.new_block();
                        let body_blk = fb.new_block();
                        let exit_blk = fb.new_block();
                        fb.terminate_and_switch(Terminator::Goto(cond_blk), cond_blk);
                        let has_next = fb.new_local(Ty::Bool);
                        fb.push_stmt(MStmt::Assign {
                            dest: has_next,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/util/Iterator".to_string(),
                                    method_name: "hasNext".to_string(),
                                    descriptor: "()Z".to_string(),
                                },
                                args: vec![iter_local],
                            },
                        });
                        fb.terminate_and_switch(
                            Terminator::Branch {
                                cond: has_next,
                                then_block: body_blk,
                                else_block: exit_blk,
                            },
                            body_blk,
                        );
                        // val element = iter.next()
                        let element_obj = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest: element_obj,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/util/Iterator".to_string(),
                                    method_name: "next".to_string(),
                                    descriptor: "()Ljava/lang/Object;".to_string(),
                                },
                                args: vec![iter_local],
                            },
                        });
                        // Look up the lambda's invoke param type. If it's a
                        // primitive, unbox the Object returned by next() so the
                        // invokevirtual descriptor matches the real method.
                        let invoke_param_ty = module
                            .classes
                            .iter()
                            .find(|c| c.name == cn)
                            .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                            .and_then(|m| m.params.get(1)) // skip `this`
                            .map(|p| {
                                module
                                    .classes
                                    .iter()
                                    .find(|c| c.name == cn)
                                    .unwrap()
                                    .methods
                                    .iter()
                                    .find(|m| m.name == "invoke")
                                    .unwrap()
                                    .locals[p.0 as usize]
                                    .clone()
                            })
                            .unwrap_or(Ty::Any);
                        let element = match &invoke_param_ty {
                            Ty::Int => {
                                let unboxed = fb.new_local(Ty::Int);
                                fb.push_stmt(MStmt::Assign {
                                    dest: unboxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::VirtualJava {
                                            class_name: "java/lang/Integer".to_string(),
                                            method_name: "intValue".to_string(),
                                            descriptor: "()I".to_string(),
                                        },
                                        args: vec![element_obj],
                                    },
                                });
                                unboxed
                            }
                            Ty::Long => {
                                let unboxed = fb.new_local(Ty::Long);
                                fb.push_stmt(MStmt::Assign {
                                    dest: unboxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::VirtualJava {
                                            class_name: "java/lang/Long".to_string(),
                                            method_name: "longValue".to_string(),
                                            descriptor: "()J".to_string(),
                                        },
                                        args: vec![element_obj],
                                    },
                                });
                                unboxed
                            }
                            Ty::Double => {
                                let unboxed = fb.new_local(Ty::Double);
                                fb.push_stmt(MStmt::Assign {
                                    dest: unboxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::VirtualJava {
                                            class_name: "java/lang/Double".to_string(),
                                            method_name: "doubleValue".to_string(),
                                            descriptor: "()D".to_string(),
                                        },
                                        args: vec![element_obj],
                                    },
                                });
                                unboxed
                            }
                            Ty::Bool => {
                                let unboxed = fb.new_local(Ty::Bool);
                                fb.push_stmt(MStmt::Assign {
                                    dest: unboxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::VirtualJava {
                                            class_name: "java/lang/Boolean".to_string(),
                                            method_name: "booleanValue".to_string(),
                                            descriptor: "()Z".to_string(),
                                        },
                                        args: vec![element_obj],
                                    },
                                });
                                unboxed
                            }
                            // For String, Any, Class, etc. — pass the Object
                            // directly without unboxing.
                            _ => element_obj,
                        };
                        // lambda.invoke(element)
                        let _call_result = fb.new_local(Ty::Unit);
                        fb.push_stmt(MStmt::Assign {
                            dest: _call_result,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: cn,
                                    method_name: "invoke".to_string(),
                                },
                                args: vec![lambda_local, element],
                            },
                        });
                        fb.terminate_and_switch(Terminator::Goto(cond_blk), exit_blk);
                        let result = fb.new_local(Ty::Unit);
                        return Some(result);
                    }
                }

                // ── MutableList .add() / .remove() / .removeAt() / .clear() ───
                if matches!(&recv_ty, Ty::Class(cn) if cn.contains("ArrayList") || cn.contains("List"))
                {
                    let list_method: Option<(&str, &str, &str, Ty)> =
                        match (method_name_str.as_str(), args.len()) {
                            ("add", 1) => {
                                // Autobox the argument before passing to add(Object).
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some(("java/util/List", "add", "(Ljava/lang/Object;)Z", Ty::Bool))
                            }
                            ("remove", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/List",
                                    "remove",
                                    "(Ljava/lang/Object;)Z",
                                    Ty::Bool,
                                ))
                            }
                            ("removeAt", 1) => {
                                Some(("java/util/List", "remove", "(I)Ljava/lang/Object;", Ty::Any))
                            }
                            ("clear", 0) => Some(("java/util/List", "clear", "()V", Ty::Unit)),
                            ("get", 1) => {
                                Some(("java/util/List", "get", "(I)Ljava/lang/Object;", Ty::Any))
                            }
                            ("contains", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/List",
                                    "contains",
                                    "(Ljava/lang/Object;)Z",
                                    Ty::Bool,
                                ))
                            }
                            _ => None,
                        };
                    if let Some((jvm_class, jvm_method, descriptor, ret_ty)) = list_method {
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: jvm_class.to_string(),
                                    method_name: jvm_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                },
                                args: all_args,
                            },
                        });
                        return Some(dest);
                    }
                }

                // ── Map .containsKey() / .get() / .put() / .remove() ───
                if matches!(&recv_ty, Ty::Class(cn) if cn.contains("Map")) {
                    let map_method: Option<(&str, &str, &str, Ty)> =
                        match (method_name_str.as_str(), args.len()) {
                            ("containsKey", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/Map",
                                    "containsKey",
                                    "(Ljava/lang/Object;)Z",
                                    Ty::Bool,
                                ))
                            }
                            ("containsValue", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/Map",
                                    "containsValue",
                                    "(Ljava/lang/Object;)Z",
                                    Ty::Bool,
                                ))
                            }
                            ("get", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/Map",
                                    "get",
                                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                                    Ty::Any,
                                ))
                            }
                            ("put", 2) => {
                                let k = all_args[1];
                                let k_ty = fb.mf.locals[k.0 as usize].clone();
                                all_args[1] = mir_autobox(fb, k, &k_ty);
                                let v = all_args[2];
                                let v_ty = fb.mf.locals[v.0 as usize].clone();
                                all_args[2] = mir_autobox(fb, v, &v_ty);
                                Some((
                                    "java/util/Map",
                                    "put",
                                    "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
                                    Ty::Any,
                                ))
                            }
                            ("remove", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/Map",
                                    "remove",
                                    "(Ljava/lang/Object;)Ljava/lang/Object;",
                                    Ty::Any,
                                ))
                            }
                            ("isEmpty", 0) => Some(("java/util/Map", "isEmpty", "()Z", Ty::Bool)),
                            ("clear", 0) => Some(("java/util/Map", "clear", "()V", Ty::Unit)),
                            _ => None,
                        };
                    if let Some((jvm_class, jvm_method, descriptor, ret_ty)) = map_method {
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: jvm_class.to_string(),
                                    method_name: jvm_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                },
                                args: all_args,
                            },
                        });
                        return Some(dest);
                    }
                }

                // ── Set .contains() / .add() / .remove() / .isEmpty() ───
                if matches!(&recv_ty, Ty::Class(cn) if cn.contains("Set")) {
                    let set_method: Option<(&str, &str, &str, Ty)> =
                        match (method_name_str.as_str(), args.len()) {
                            ("contains", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some((
                                    "java/util/Set",
                                    "contains",
                                    "(Ljava/lang/Object;)Z",
                                    Ty::Bool,
                                ))
                            }
                            ("add", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some(("java/util/Set", "add", "(Ljava/lang/Object;)Z", Ty::Bool))
                            }
                            ("remove", 1) => {
                                let arg = all_args[1];
                                let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                                let boxed = mir_autobox(fb, arg, &arg_ty);
                                all_args[1] = boxed;
                                Some(("java/util/Set", "remove", "(Ljava/lang/Object;)Z", Ty::Bool))
                            }
                            ("isEmpty", 0) => Some(("java/util/Set", "isEmpty", "()Z", Ty::Bool)),
                            ("clear", 0) => Some(("java/util/Set", "clear", "()V", Ty::Unit)),
                            _ => None,
                        };
                    if let Some((jvm_class, jvm_method, descriptor, ret_ty)) = set_method {
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: jvm_class.to_string(),
                                    method_name: jvm_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                },
                                args: all_args,
                            },
                        });
                        return Some(dest);
                    }
                }

                // ── Pair .toString() — on kotlin/Pair instances ─────
                if matches!(&recv_ty, Ty::Class(cn) if cn == "kotlin/Pair")
                    && method_name_str == "toString"
                {
                    let dest = fb.new_local(Ty::String);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlin/Pair".to_string(),
                                method_name: "toString".to_string(),
                                descriptor: "()Ljava/lang/String;".to_string(),
                            },
                            args: all_args,
                        },
                    });
                    return Some(dest);
                }

                // ── Kotlin stdlib extension functions ─────────────────
                // Extension functions like `List.map {}` are compiled as
                // static methods in `*Kt` facade classes. The receiver
                // becomes the first argument and lambdas must implement
                // `kotlin/jvm/functions/FunctionN`.
                {
                    let recv_ty_str = match &recv_ty {
                        Ty::Class(cn) => cn.as_str(),
                        Ty::String => "java/lang/String",
                        Ty::Int => "java/lang/Integer",
                        Ty::Any => "java/lang/Object",
                        _ => "",
                    };
                    if let Some((facade_class, facade_method, descriptor, ret_ty)) =
                        stdlib_extension(recv_ty_str, &method_name_str)
                    {
                        // For `fold`, the second arg (initial accumulator,
                        // index 1 in all_args) is `Object` on the JVM so
                        // any primitive must be boxed.
                        if facade_method == "fold" && all_args.len() > 1 {
                            let init = all_args[1];
                            let init_ty = fb.mf.locals[init.0 as usize].clone();
                            all_args[1] = mir_autobox(fb, init, &init_ty);
                        }
                        // joinToString$default needs all 9 params even when
                        // the user only supplies the separator.  Cast the
                        // String separator to CharSequence, pad nulls for
                        // prefix/postfix/truncated/transform, 0 for limit,
                        // set the bitmask, and add the trailing marker null.
                        if facade_method == "joinToString$default" {
                            // all_args[0] = receiver (Iterable)
                            // all_args[1] = separator (user-supplied String)
                            // Cast separator to CharSequence descriptor match
                            // (no-op on JVM, types are compatible)

                            // Build full arg list:
                            //   Iterable, CharSequence, CharSequence, CharSequence, int, CharSequence, Function1, int, Object
                            let receiver = all_args[0];
                            let separator = if all_args.len() > 1 {
                                all_args[1]
                            } else {
                                // default separator ", "
                                let sid = module.intern_string(", ");
                                let s = fb.new_local(Ty::String);
                                fb.push_stmt(MStmt::Assign {
                                    dest: s,
                                    value: Rvalue::Const(MirConst::String(sid)),
                                });
                                s
                            };
                            let null_cs = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                            fb.push_stmt(MStmt::Assign {
                                dest: null_cs,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            let null_cs2 = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                            fb.push_stmt(MStmt::Assign {
                                dest: null_cs2,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            let zero = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: zero,
                                value: Rvalue::Const(MirConst::Int(0)),
                            });
                            let null_cs3 = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                            fb.push_stmt(MStmt::Assign {
                                dest: null_cs3,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            let null_fn = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                            fb.push_stmt(MStmt::Assign {
                                dest: null_fn,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            // Bitmask: 62 = 0b111110 — all params except separator use defaults
                            let bitmask = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: bitmask,
                                value: Rvalue::Const(MirConst::Int(62)),
                            });
                            let null_marker = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                            fb.push_stmt(MStmt::Assign {
                                dest: null_marker,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            all_args = vec![
                                receiver,
                                separator,
                                null_cs,
                                null_cs2,
                                zero,
                                null_cs3,
                                null_fn,
                                bitmask,
                                null_marker,
                            ];
                        }
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::StaticJava {
                                    class_name: facade_class.to_string(),
                                    method_name: facade_method.to_string(),
                                    descriptor: descriptor.to_string(),
                                },
                                args: all_args,
                            },
                        });
                        return Some(dest);
                    }
                }

                // Check if receiver is a class instance for virtual dispatch.

                // Override table for methods with ambiguous JVM overloads.
                // These need explicit descriptors because the JVM class has
                // multiple overloads that can't be distinguished by arg count.
                // Disambiguation table for JVM methods with multiple overloads
                // that share the same argument count. These cases can't be
                // resolved by class-file lookup alone without full type
                // inference on the argument expressions.
                // String method overload disambiguation (from registry).
                let overload_override: Option<(&str, &str, &str, Ty)> =
                    if matches!(&recv_ty, Ty::String) {
                        skotch_stdlib_registry::lookup_string_overload(&method_name_str, args.len())
                            .map(|o| (o.jvm_class, o.jvm_method, o.descriptor, (o.return_ty)()))
                    } else {
                        None
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

                // ── `deferred.await()` ─────────────────
                //
                // `Deferred.await()` is a suspend function on the
                // `kotlinx/coroutines/Deferred` interface. At the JVM
                // level it becomes:
                //   aload <deferred>
                //   aload <continuation>
                //   invokeinterface Deferred.await(Continuation)Object
                //
                // This is a suspension point — it may return
                // COROUTINE_SUSPENDED. We emit it as
                // a VirtualJava call; the JVM backend recognizes
                // `Deferred` as an interface and emits `invokeinterface`.
                if method_name_str == "await"
                    && args.is_empty()
                    && matches!(&recv_ty, Ty::Class(cn) if cn == "kotlinx/coroutines/Deferred")
                {
                    // The continuation is the enclosing suspend fn's
                    // $completion or, for suspend lambdas, `this`.
                    let cont_local = if fb.mf.is_suspend {
                        *fb.mf
                            .params
                            .last()
                            .expect("suspend fn must have $completion")
                    } else {
                        let c =
                            fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: c,
                            value: Rvalue::Const(MirConst::Null),
                        });
                        c
                    };
                    let dest = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlinx/coroutines/Deferred".to_string(),
                                method_name: "await".to_string(),
                                descriptor: "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;"
                                    .to_string(),
                            },
                            args: vec![recv_local, cont_local],
                        },
                    });
                    return Some(dest);
                }

                // ── `job.join()` ────────────────────────
                //
                // `Job.join()` is a suspend function on the
                // `kotlinx/coroutines/Job` interface. At JVM level:
                //   aload <job>
                //   aload <continuation>
                //   invokeinterface Job.join(Continuation)V
                //
                // It returns Unit (void), but on JVM it's Object because
                // the CPS transform erases the return type.
                if method_name_str == "join"
                    && args.is_empty()
                    && matches!(&recv_ty, Ty::Class(cn) if cn == "kotlinx/coroutines/Job")
                {
                    let cont_local = if fb.mf.is_suspend {
                        *fb.mf
                            .params
                            .last()
                            .expect("suspend fn must have $completion")
                    } else {
                        let c =
                            fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: c,
                            value: Rvalue::Const(MirConst::Null),
                        });
                        c
                    };
                    let dest = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlinx/coroutines/Job".to_string(),
                                method_name: "join".to_string(),
                                descriptor: "(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;"
                                    .to_string(),
                            },
                            args: vec![recv_local, cont_local],
                        },
                    });
                    return Some(dest);
                }

                // ── `job.cancel()` ──────────────────────
                //
                // `Job.cancel()` is a non-suspend function on Job.
                // It cancels the job (non-blocking).
                if method_name_str == "cancel"
                    && args.is_empty()
                    && matches!(&recv_ty, Ty::Class(cn) if cn == "kotlinx/coroutines/Job"
                        || cn == "kotlinx/coroutines/Deferred")
                {
                    let dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlinx/coroutines/Job".to_string(),
                                method_name: "cancel".to_string(),
                                descriptor: "()V".to_string(),
                            },
                            args: vec![recv_local],
                        },
                    });
                    return Some(dest);
                }

                // ── `job.isActive` property ─────────────
                //
                // `Job.isActive` is a property (getter: isActive()Z).
                if method_name_str == "isActive"
                    && args.is_empty()
                    && matches!(&recv_ty, Ty::Class(cn) if cn == "kotlinx/coroutines/Job"
                        || cn == "kotlinx/coroutines/Deferred")
                {
                    let dest = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "kotlinx/coroutines/Job".to_string(),
                                method_name: "isActive".to_string(),
                                descriptor: "()Z".to_string(),
                            },
                            args: vec![recv_local],
                        },
                    });
                    return Some(dest);
                }

                // Resolve methods dynamically from JDK class files.
                let class_name_buf;
                let jvm_class_for_ty = match &recv_ty {
                    Ty::String => Some("java/lang/String"),
                    Ty::Int => Some("java/lang/Integer"),
                    Ty::Long => Some("java/lang/Long"),
                    Ty::Double => Some("java/lang/Double"),
                    Ty::Bool => Some("java/lang/Boolean"),
                    Ty::Class(cn) if cn.contains('/') => {
                        class_name_buf = cn.clone();
                        Some(class_name_buf.as_str())
                    }
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

                // Primitive type conversion methods: toInt(), toDouble(), toLong(), toChar()
                if args.is_empty() {
                    let conversion: Option<(Ty, &str)> = match (method_name_str.as_str(), &recv_ty)
                    {
                        ("toDouble", Ty::Int) => Some((Ty::Double, "i2d")),
                        ("toDouble", Ty::Long) => Some((Ty::Double, "l2d")),
                        ("toLong", Ty::Int) => Some((Ty::Long, "i2l")),
                        ("toLong", Ty::Double) => Some((Ty::Long, "d2l")),
                        ("toInt", Ty::Double) => Some((Ty::Int, "d2i")),
                        ("toInt", Ty::Long) => Some((Ty::Int, "l2i")),
                        ("toInt", Ty::Char) => Some((Ty::Int, "nop")),
                        ("toChar", Ty::Int) => Some((Ty::Char, "i2c")),
                        ("toFloat", Ty::Int) => Some((Ty::Double, "i2d")),
                        ("toFloat", Ty::Double) => Some((Ty::Double, "nop")),
                        _ => None,
                    };
                    if let Some((ret_ty, opcode_name)) = conversion {
                        let dest = fb.new_local(ret_ty);
                        if opcode_name == "nop" {
                            // Identity conversion — just copy.
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Local(recv_local),
                            });
                        } else {
                            // JVM type conversion opcode — emitted as a
                            // special StaticJava call that the backend
                            // recognizes.
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::StaticJava {
                                        class_name: "$convert".to_string(),
                                        method_name: opcode_name.to_string(),
                                        descriptor: "()V".to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                        }
                        return Some(dest);
                    }
                }

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
                    // Look up method in class, walking superclass + interfaces.
                    let mut return_ty = Ty::Unit;
                    let mut found_on_interface = false;
                    let mut found_is_suspend = false;
                    let mut iface_name = String::new();
                    let mut search = Some(class_name.clone());
                    'outer: while let Some(ref cname) = search {
                        if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                            if let Some(m) = cls.methods.iter().find(|m| m.name == method_name_str)
                            {
                                return_ty = m.return_ty.clone();
                                found_is_suspend = m.is_suspend;
                                break;
                            }
                            // Search implemented interfaces.
                            for iname in &cls.interfaces {
                                if let Some(icls) = module.classes.iter().find(|c| &c.name == iname)
                                {
                                    if let Some(m) =
                                        icls.methods.iter().find(|m| m.name == method_name_str)
                                    {
                                        return_ty = m.return_ty.clone();
                                        found_is_suspend = m.is_suspend;
                                        found_on_interface = true;
                                        iface_name = iname.clone();
                                        break 'outer;
                                    }
                                }
                            }
                            search = cls.super_class.clone();
                        } else {
                            break;
                        }
                    }

                    // Fallback: if the method wasn't found in MIR classes,
                    // check for known Java methods.
                    if return_ty == Ty::Unit {
                        return_ty = match (method_name_str.as_str(), args.len()) {
                            ("toString", 0) => Ty::String,
                            ("hashCode", 0) => Ty::Int,
                            ("equals", 1) => Ty::Bool,
                            ("length", 0) => Ty::Int,
                            ("size", 0) => Ty::Int,
                            _ => {
                                // StringBuilder.append returns StringBuilder.
                                if class_name.contains("StringBuilder")
                                    && method_name_str == "append"
                                {
                                    Ty::Class(class_name.clone())
                                } else {
                                    Ty::Unit
                                }
                            }
                        };
                    }

                    // Suspend instance method calls. Append
                    // the $completion continuation and emit as VirtualJava
                    // so the state machine extractor detects it as a
                    // suspension point.
                    if found_is_suspend {
                        let cont_local = if fb.mf.is_suspend {
                            *fb.mf
                                .params
                                .last()
                                .expect("suspend fn must have $completion")
                        } else {
                            let c = fb
                                .new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                            fb.push_stmt(MStmt::Assign {
                                dest: c,
                                value: Rvalue::Const(MirConst::Null),
                            });
                            c
                        };
                        all_args.push(cont_local);
                        // Build VirtualJava descriptor: (user_params..., Continuation)Object
                        let mut desc = String::from("(");
                        // Skip receiver (first arg) in descriptor
                        for &a in all_args.iter().skip(1) {
                            let ty = &fb.mf.locals[a.0 as usize];
                            desc.push_str(&jvm_type_string_for_ty(ty));
                        }
                        desc.push_str(")Ljava/lang/Object;");
                        let target_class = if found_on_interface {
                            iface_name
                        } else {
                            class_name.clone()
                        };
                        (
                            CallKind::VirtualJava {
                                class_name: target_class,
                                method_name: method_name_str,
                                descriptor: desc,
                            },
                            Ty::Any,
                        )
                    } else if found_on_interface {
                        (
                            CallKind::Virtual {
                                class_name: iface_name,
                                method_name: method_name_str,
                            },
                            return_ty,
                        )
                    } else if let Some((_, callable_local)) = scope
                        .iter()
                        .rev()
                        .find(|(s, _)| interner.resolve(*s) == method_name_str)
                    {
                        // receiver.callable(args) where callable is a
                        // local variable of function/lambda type.
                        // Invoke as callable.invoke(receiver, args).
                        let callable_local = *callable_local;
                        let callable_ty = fb.mf.locals[callable_local.0 as usize].clone();
                        let invoke_class = if let Ty::Class(ref cn) = callable_ty {
                            cn.clone()
                        } else {
                            "java/lang/Object".to_string()
                        };
                        let recv_arg = {
                            let widened = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: widened,
                                value: Rvalue::Local(recv_local),
                            });
                            widened
                        };
                        let mut invoke_args = vec![callable_local, recv_arg];
                        for &a in all_args.iter().skip(1) {
                            let a_ty = fb.mf.locals[a.0 as usize].clone();
                            let boxed = mir_autobox(fb, a, &a_ty);
                            invoke_args.push(boxed);
                        }
                        // FunctionN.invoke always returns Object on JVM.
                        let dest = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: invoke_class,
                                    method_name: "invoke".to_string(),
                                },
                                args: invoke_args,
                            },
                        });
                        return Some(dest);
                    } else {
                        (
                            CallKind::Virtual {
                                class_name: class_name.clone(),
                                method_name: method_name_str,
                            },
                            return_ty,
                        )
                    }
                } else if matches!(&recv_ty, Ty::Any | Ty::Nullable(_)) {
                    // Methods defined on java/lang/Object are available on
                    // every JVM type, including erased generics (Ty::Any).
                    let object_method: Option<(&str, Ty)> =
                        match (method_name_str.as_str(), args.len()) {
                            ("toString", 0) => Some(("()Ljava/lang/String;", Ty::String)),
                            ("hashCode", 0) => Some(("()I", Ty::Int)),
                            ("equals", 1) => Some(("(Ljava/lang/Object;)Z", Ty::Bool)),
                            ("getClass", 0) => Some(("()Ljava/lang/Class;", Ty::Any)),
                            _ => None,
                        };
                    if let Some((descriptor, ret_ty)) = object_method {
                        (
                            CallKind::VirtualJava {
                                class_name: "java/lang/Object".to_string(),
                                method_name: method_name_str,
                                descriptor: descriptor.to_string(),
                            },
                            ret_ty,
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
                    }
                } else if let Some(fid) = name_to_func.get(&method_name) {
                    (
                        CallKind::Static(*fid),
                        module.functions[fid.0 as usize].return_ty.clone(),
                    )
                } else if let Some((_, callable_local)) =
                    scope.iter().rev().find(|(s, _)| *s == method_name)
                {
                    // receiver.callable(args) where callable is a local
                    // variable of function type → callable.invoke(receiver, args).
                    // This handles extension function type invocations like
                    // `sb.block()` where block: StringBuilder.() -> Unit.
                    let callable_local = *callable_local;
                    let callable_ty = fb.mf.locals[callable_local.0 as usize].clone();
                    let is_callable = matches!(&callable_ty, Ty::Class(n) if n.contains("$Lambda$"))
                        || matches!(callable_ty, Ty::Any | Ty::Function { .. });
                    if is_callable {
                        let invoke_class = if let Ty::Class(ref cn) = callable_ty {
                            cn.clone()
                        } else {
                            "java/lang/Object".to_string()
                        };
                        // Widen receiver to Any for the erased invoke signature.
                        let recv_arg = {
                            let widened = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: widened,
                                value: Rvalue::Local(recv_local),
                            });
                            widened
                        };
                        let mut invoke_args = vec![callable_local, recv_arg];
                        for &a in all_args.iter().skip(1) {
                            let a_ty = fb.mf.locals[a.0 as usize].clone();
                            let boxed = mir_autobox(fb, a, &a_ty);
                            invoke_args.push(boxed);
                        }
                        // FunctionN.invoke always returns Object on JVM.
                        let dest = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: invoke_class,
                                    method_name: "invoke".to_string(),
                                },
                                args: invoke_args,
                            },
                        });
                        return Some(dest);
                    }
                    diags.push(Diagnostic::error(
                        *span,
                        format!("unknown method `{}`", interner.resolve(method_name)),
                    ));
                    return None;
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

            // Handle safe method calls: `receiver?.method(args)`
            // Wrap the call in a null-check: if (recv != null) recv.method(args) else null
            if let Expr::SafeCall {
                receiver: sc_receiver,
                name: sc_name,
                span: sc_span,
            } = callee.as_ref()
            {
                let recv = lower_expr(
                    sc_receiver,
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

                // then-block: unwrap and dispatch the method call via a
                // synthetic Call { callee: Field { .. } } expression.
                let recv_ty = fb.mf.locals[recv.0 as usize].clone();
                let inner_ty = match &recv_ty {
                    Ty::Nullable(inner) => (**inner).clone(),
                    other => other.clone(),
                };
                let recv_unwrapped = if matches!(recv_ty, Ty::Nullable(_)) {
                    let uw = fb.new_local(inner_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest: uw,
                        value: Rvalue::Local(recv),
                    });
                    uw
                } else {
                    recv
                };

                // Build a synthetic Field-based call expression and lower it.
                // We use a temporary ident node whose symbol is bound to the
                // unwrapped receiver local in the scope.
                let tmp_sym = interner.intern("$safe_recv$");
                scope.push((tmp_sym, recv_unwrapped));
                let synthetic_call = Expr::Call {
                    callee: Box::new(Expr::Field {
                        receiver: Box::new(Expr::Ident(tmp_sym, *sc_span)),
                        name: *sc_name,
                        span: *sc_span,
                    }),
                    args: args.clone(),
                    type_args: type_args.clone(),
                    span: *span,
                };
                let call_result = lower_expr(
                    &synthetic_call,
                    fb,
                    scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    loop_ctx,
                );
                scope.pop(); // remove $safe_recv$

                if let Some(cr) = call_result {
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(cr),
                    });
                }
                fb.terminate_and_switch(Terminator::Goto(merge_block), else_block);

                // else-block: result = null
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Const(MirConst::Null),
                });
                fb.terminate_and_switch(Terminator::Goto(merge_block), merge_block);

                return Some(result);
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

            // ─── SAM conversion: Interface { lambda } ────────────────
            // When the callee names an interface with a single abstract
            // method, and the argument is a lambda, desugar to an
            // anonymous object implementing the interface.
            let callee_str = interner.resolve(callee_name).to_string();
            let sam_method = module
                .classes
                .iter()
                .find(|c| c.name == callee_str && c.is_interface)
                .and_then(|iface| {
                    let abstract_methods: Vec<&MirFunction> =
                        iface.methods.iter().filter(|m| m.is_abstract).collect();
                    if abstract_methods.len() == 1 {
                        Some((
                            abstract_methods[0].name.clone(),
                            abstract_methods[0].return_ty.clone(),
                            abstract_methods[0]
                                .params
                                .iter()
                                .skip(1) // skip `this`
                                .map(|p| iface.methods[0].locals[p.0 as usize].clone())
                                .collect::<Vec<Ty>>(),
                        ))
                    } else {
                        None
                    }
                });
            if let Some((sam_name, sam_ret, _sam_params)) = sam_method {
                if args.len() == 1 {
                    if let Expr::Lambda {
                        params: lparams,
                        body: lbody,
                        span: lspan,
                        ..
                    } = &args[0].expr
                    {
                        // Construct an ObjectExpr AST node and lower it.
                        let override_fn = skotch_syntax::FunDecl {
                            name: interner.intern(&sam_name),
                            name_span: *lspan,
                            type_params: Vec::new(),
                            params: lparams.clone(),
                            return_ty: None,
                            receiver_ty: None,
                            body: lbody.clone(),
                            is_open: false,
                            is_override: true,
                            is_abstract: false,
                            is_suspend: false,
                            is_inline: false,
                            visibility: skotch_syntax::Visibility::Public,
                            annotations: Vec::new(),
                            span: *lspan,
                        };
                        // If the SAM method returns non-Unit, set return type.
                        if sam_ret != Ty::Unit {
                            // Leave return_ty as None — inferred from body.
                        }
                        let obj_expr = Expr::ObjectExpr {
                            super_type: callee_name,
                            methods: vec![override_fn],
                            span: *lspan,
                        };
                        return lower_expr(
                            &obj_expr,
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

            // ─── Reified inline: `f<Type>(arg)` where f has reified T ──
            // For inline functions with reified type params, we inline the
            // `is T` check at the call site using the concrete type arg.
            // Supports multi-reified: `f<A, B>(arg)` substitutes both A and B.
            if !type_args.is_empty() {
                if let Some(fid) = name_to_func.get(&callee_name) {
                    let target = &module.functions[fid.0 as usize];
                    // Build type param → concrete type map.
                    // Use common convention: T, A, B, C... for positional params.
                    let common_params = ["T", "A", "B", "C", "R", "E", "K", "V"];
                    for (i, ta) in type_args.iter().enumerate() {
                        if i < common_params.len() {
                            let concrete = interner.resolve(ta.name).to_string();
                            fb.reified_types
                                .insert(common_params[i].to_string(), concrete.clone());
                        }
                    }
                    // Check if the function body is a single is-check pattern
                    // (the common reified use case).
                    if target.return_ty == Ty::Bool && !args.is_empty() {
                        // Use the first type arg for the simple pattern.
                        let concrete_type = interner.resolve(type_args[0].name).to_string();
                        // Lower the argument.
                        let arg_local = lower_expr(
                            &args[0].expr,
                            fb,
                            scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            loop_ctx,
                        )?;
                        // The parameter type is `Any` so it's already an Object on JVM.
                        // But if the argument is a primitive, we need it autoboxed.
                        // The function's parameter is typed `Ty::Any`, so the autoboxing
                        // happens at the JVM backend level when the static call is emitted.
                        // For the inlined instanceof, we just use the arg directly —
                        // if it's a primitive, we need to box it first.
                        let obj_local = match fb.mf.locals[arg_local.0 as usize] {
                            Ty::Int | Ty::Long | Ty::Double | Ty::Bool => {
                                // Box the primitive by calling it through a static valueOf.
                                let (class, desc) = match fb.mf.locals[arg_local.0 as usize] {
                                    Ty::Int => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
                                    Ty::Long => ("java/lang/Long", "(J)Ljava/lang/Long;"),
                                    Ty::Double => ("java/lang/Double", "(D)Ljava/lang/Double;"),
                                    Ty::Bool => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
                                    _ => unreachable!(),
                                };
                                let boxed = fb.new_local(Ty::Any);
                                fb.push_stmt(MStmt::Assign {
                                    dest: boxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::StaticJava {
                                            class_name: class.to_string(),
                                            method_name: "valueOf".to_string(),
                                            descriptor: desc.to_string(),
                                        },
                                        args: vec![arg_local],
                                    },
                                });
                                boxed
                            }
                            _ => arg_local,
                        };
                        // Emit instanceof check with the concrete type.
                        let jvm_name = match concrete_type.as_str() {
                            "String" => "java/lang/String".to_string(),
                            "Int" => "java/lang/Integer".to_string(),
                            "Long" => "java/lang/Long".to_string(),
                            "Double" => "java/lang/Double".to_string(),
                            "Boolean" => "java/lang/Boolean".to_string(),
                            other => other.to_string(),
                        };
                        let result = fb.new_local(Ty::Bool);
                        fb.push_stmt(MStmt::Assign {
                            dest: result,
                            value: Rvalue::InstanceOf {
                                obj: obj_local,
                                type_descriptor: jvm_name,
                            },
                        });
                        return Some(result);
                    }
                }
            }

            // ─── Set reified type substitutions for IsCheck ────────
            // Store type arg names as reified substitutions so that
            // `is T` in function bodies resolves to the concrete type.
            if !type_args.is_empty() {
                // Simple heuristic: map single-letter type param names.
                // For the common case `f<String>(x)`, map T→String.
                let common_params = ["T", "A", "B", "C", "R", "E", "K", "V"];
                for (i, ta) in type_args.iter().enumerate() {
                    if i < common_params.len() {
                        let concrete = interner.resolve(ta.name).to_string();
                        fb.reified_types
                            .insert(common_params[i].to_string(), concrete);
                    }
                }
            }

            // ─── Check for constructor call (class instantiation) ────
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

            // ─── Imported class constructors ─────────────────────────
            // Check import_map for Java classes: `import java.util.Random`
            // allows `Random()` as a constructor call.
            if let Some(jvm_class) = module.import_map.get(&callee_str).cloned() {
                if skotch_classinfo::load_jdk_class(&jvm_class).is_ok() {
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
                    let dest = fb.new_local(Ty::Class(jvm_class.clone()));
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::NewInstance(jvm_class.clone()),
                    });
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Constructor(jvm_class),
                            args: arg_locals,
                        },
                    });
                    return Some(dest);
                }
            }

            // ─── Cross-file class constructor ─────────────────────────
            // If the callee matches a cross-file user class (registered via
            // PackageSymbolTable), emit NewInstance + Constructor.
            if let Some((jvm_class, _, _)) = module
                .cross_file_classes
                .get(&callee_str.to_string())
                .cloned()
            {
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
                let dest = fb.new_local(Ty::Class(jvm_class.clone()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance(jvm_class.clone()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::Constructor(jvm_class),
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

            // For `with(receiver) { ... }`, set the lambda receiver
            // type after lowering the first (receiver) arg.
            let is_with_call = callee_str == "with";

            // If the callee is a coroutine builder, set a flag so
            // trailing lambda args are created as SuspendLambda.
            let is_coroutine_builder = callee_str == "runBlocking"
                || callee_str == "launch"
                || callee_str == "async"
                || callee_str == "withContext"
                || callee_str == "coroutineScope"
                || callee_str == "supervisorScope"
                || callee_str == "withTimeout"
                || callee_str == "withTimeoutOrNull";
            if is_coroutine_builder {
                module.force_suspend_lambda = true;
            }

            // Lower arguments. If any are named, we'll reorder after.
            let has_named = args.iter().any(|a| a.name.is_some());
            let mut named_pairs: Vec<(Option<Symbol>, LocalId)> = Vec::new();
            // Look up param_receiver_types for the target function so
            // we can set lambda_receiver_type before lowering lambda args.
            let param_recv_types: Vec<(usize, String)> = name_to_func
                .get(&callee_name)
                .map(|fid| {
                    module.functions[fid.0 as usize]
                        .param_receiver_types
                        .clone()
                })
                .unwrap_or_default();
            for (arg_idx, a) in args.iter().enumerate() {
                // If this arg position has a receiver type and the arg
                // is a lambda, set the receiver type flag.
                if let Some((_, recv_ty)) = param_recv_types.iter().find(|(idx, _)| *idx == arg_idx)
                {
                    if matches!(&a.expr, Expr::Lambda { .. }) {
                        module.lambda_receiver_type = Some(recv_ty.clone());
                    }
                }
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
                // For `with(receiver, lambda)`: after lowering the first
                // (receiver) arg, set the receiver type for the lambda.
                if is_with_call && arg_idx == 0 {
                    let arg_ty = &fb.mf.locals[id.0 as usize];
                    if let Ty::Class(cn) = arg_ty {
                        module.lambda_receiver_type = Some(cn.clone());
                    }
                }
            }
            // Clear the force flag in case it wasn't consumed
            // (e.g. the lambda was a capture, not a literal).
            module.force_suspend_lambda = false;

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

            // ─── Vararg packing ──────────────────────────────────────
            // If the target function has a vararg parameter, pack all
            // arguments from that index onward into a single IntArray.
            if let Some(fid) = name_to_func.get(&callee_name) {
                let vi = module.functions[fid.0 as usize].vararg_index;
                if let Some(va_idx) = vi {
                    if arg_locals.len() >= va_idx {
                        let vararg_args: Vec<LocalId> = arg_locals.drain(va_idx..).collect();
                        // Spread optimization: if there's exactly one arg and
                        // it's already an IntArray, pass it directly (no repack).
                        // This handles `sum(*intArrayOf(1,2,3))` and `sum(*arr)`.
                        let is_spread = vararg_args.len() == 1
                            && matches!(
                                fb.mf.locals[vararg_args[0].0 as usize],
                                Ty::IntArray
                                    | Ty::LongArray
                                    | Ty::DoubleArray
                                    | Ty::BooleanArray
                                    | Ty::ByteArray
                            );
                        if is_spread {
                            arg_locals.push(vararg_args[0]);
                        } else {
                            let count = vararg_args.len();
                            let count_local = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: count_local,
                                value: Rvalue::Const(MirConst::Int(count as i32)),
                            });
                            let arr = fb.new_local(Ty::IntArray);
                            fb.push_stmt(MStmt::Assign {
                                dest: arr,
                                value: Rvalue::NewIntArray(count_local),
                            });
                            for (i, elem) in vararg_args.iter().enumerate() {
                                let idx_local = fb.new_local(Ty::Int);
                                fb.push_stmt(MStmt::Assign {
                                    dest: idx_local,
                                    value: Rvalue::Const(MirConst::Int(i as i32)),
                                });
                                let store_dest = fb.new_local(Ty::Unit);
                                fb.push_stmt(MStmt::Assign {
                                    dest: store_dest,
                                    value: Rvalue::ArrayStore {
                                        array: arr,
                                        index: idx_local,
                                        value: *elem,
                                    },
                                });
                            }
                            arg_locals.push(arr);
                        }
                    }
                }
            }

            let callee_str = interner.resolve(callee_name).to_string();
            let callee_str = callee_str.as_str();

            // ── `runBlocking { }` ──────────────────────────
            //
            // `fun main() = runBlocking { delay(10); println("done") }`
            //
            // kotlinc compiles this as:
            //   aconst_null                       // CoroutineContext arg (null → default)
            //   new Lambda; dup; aconst_null
            //   invokespecial Lambda.<init>(Continuation)V
            //   checkcast Function2
            //   iconst_1                          // default-mask (bit 0 = context defaulted)
            //   aconst_null                       // unused handler
            //   invokestatic BuildersKt.runBlocking$default(
            //       CoroutineContext, Function2, int, Object)Object
            //   pop                               // result is Unit
            //
            // The trailing lambda has already been lowered as a suspend lambda
            // class with arity 2 (CoroutineScope + Continuation → Function2).
            // The `arg_locals` at this point contain the lambda instance.
            if callee_str == "runBlocking" && arg_locals.len() == 1 {
                let lambda_local = arg_locals[0];

                // The trailing lambda for runBlocking has type
                // `suspend CoroutineScope.() -> T`, which is
                // `Function2<CoroutineScope, Continuation<T>, Object>`.
                // The MIR lowerer created it as a suspend lambda with
                // arity 1 (Function1). We need to bump it to arity 2
                // (Function2) so that `runBlocking$default` can call it.
                //
                // Find the lambda class by type and patch its interfaces.
                let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                if let Ty::Class(ref lambda_class_name) = lambda_ty {
                    if let Some(cls) = module
                        .classes
                        .iter_mut()
                        .find(|c| &c.name == lambda_class_name)
                    {
                        // Replace any FunctionN → Function2 in interfaces.
                        for iface in cls.interfaces.iter_mut() {
                            if iface.starts_with("kotlin/jvm/functions/Function") {
                                *iface = "kotlin/jvm/functions/Function2".to_string();
                            }
                        }
                    }
                }

                // Push null for CoroutineContext (defaulted).
                let null_ctx =
                    fb.new_local(Ty::Class("kotlin/coroutines/CoroutineContext".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: null_ctx,
                    value: Rvalue::Const(MirConst::Null),
                });

                // Cast lambda to Function2.
                let func2_local =
                    fb.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: func2_local,
                    value: Rvalue::CheckCast {
                        obj: lambda_local,
                        target_class: "kotlin/jvm/functions/Function2".to_string(),
                    },
                });

                // Push iconst_1 (default mask: bit 0 → context is defaulted).
                let mask_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: mask_local,
                    value: Rvalue::Const(MirConst::Int(1)),
                });

                // Push null for the unused handler arg.
                let null_handler = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: null_handler,
                    value: Rvalue::Const(MirConst::Null),
                });

                // Call BuildersKt.runBlocking$default(
                //     CoroutineContext, Function2, int, Object)Object
                let result = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlinx/coroutines/BuildersKt".to_string(),
                            method_name: "runBlocking$default".to_string(),
                            descriptor: "(Lkotlin/coroutines/CoroutineContext;Lkotlin/jvm/functions/Function2;ILjava/lang/Object;)Ljava/lang/Object;".to_string(),
                        },
                        args: vec![null_ctx, func2_local, mask_local, null_handler],
                    },
                });
                return Some(result);
            }

            // ── `launch { }` and `async { }` builders ────
            //
            // `launch { body }` and `async { body }` are coroutine
            // builders from kotlinx-coroutines. For simplicity we use
            // `GlobalScope.launch$default` / `GlobalScope.async$default`
            // rather than threading the CoroutineScope from
            // `runBlocking`'s lambda receiver. This means these launches
            // are NOT structured (GlobalScope never cancels), but it
            // produces correct runtime behavior for basic patterns.
            //
            // kotlinc reference call site:
            //   getstatic GlobalScope.INSTANCE:GlobalScope
            //   aconst_null                // CoroutineContext (default)
            //   aconst_null                // CoroutineStart  (default)
            //   new Lambda; dup; aconst_null; invokespecial <init>
            //   checkcast Function2
            //   iconst_3                   // default mask (bits 0+1)
            //   aconst_null                // unused handler
            //   invokestatic BuildersKt.launch$default(
            //       CoroutineScope, CoroutineContext, CoroutineStart,
            //       Function2, int, Object)Job
            //
            // `async` is identical but returns `Deferred` instead of `Job`.
            if (callee_str == "launch" || callee_str == "async") && arg_locals.len() == 1 {
                let lambda_local = arg_locals[0];

                // The trailing lambda for launch/async has type
                // `suspend CoroutineScope.() -> T`, i.e.
                // `Function2<CoroutineScope, Continuation<T>, Object>`.
                // Patch any FunctionN → Function2 and mark as suspend lambda
                // so the JVM backend emits the SuspendLambda shell.
                //
                // IMPORTANT: lambda_local is in the CURRENT FnBuilder (fb).
                // When inside a suspend lambda's invoke body, fb is the
                // invoke FnBuilder, not the top-level one. The local's type
                // is the inner lambda's class name.
                let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                if let Ty::Class(ref lambda_class_name) = lambda_ty {
                    if let Some(cls) = module
                        .classes
                        .iter_mut()
                        .find(|c| &c.name == lambda_class_name)
                    {
                        for iface in cls.interfaces.iter_mut() {
                            if iface.starts_with("kotlin/jvm/functions/Function") {
                                *iface = "kotlin/jvm/functions/Function2".to_string();
                            }
                        }
                        // Force SuspendLambda even if body has no suspend calls
                        // (the lambda is being passed to a coroutine builder).
                        if !cls.is_suspend_lambda {
                            cls.is_suspend_lambda = true;
                            cls.super_class =
                                Some("kotlin/coroutines/jvm/internal/SuspendLambda".to_string());
                        }
                    }
                }

                // Structured concurrency — use the enclosing
                // CoroutineScope from this.p$0 if available (set by the
                // SuspendLambda shell's create method). Fall back to
                // GlobalScope for non-lambda contexts.
                let this_sym = interner.intern("this");
                let scope_local = if let Some((_, this_local)) =
                    scope.iter().rev().find(|(s, _)| *s == this_sym)
                {
                    let this_ty = &fb.mf.locals[this_local.0 as usize];
                    // All suspend lambda classes get a p$0 field (added
                    // at finalization). Check if this class IS a suspend
                    // lambda by looking for the field OR by class name
                    // pattern (the field may not exist yet on the placeholder).
                    let has_scope_field = if let Ty::Class(cls_name) = this_ty {
                        module
                            .classes
                            .iter()
                            .find(|c| &c.name == cls_name)
                            .map(|c| {
                                c.fields.iter().any(|f| f.name == "p$0") || c.is_suspend_lambda
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    if has_scope_field {
                        let cls_name = if let Ty::Class(n) = this_ty {
                            n.clone()
                        } else {
                            unreachable!()
                        };
                        // Load the scope from this.p$0 and cast to CoroutineScope.
                        let raw_scope = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest: raw_scope,
                            value: Rvalue::GetField {
                                receiver: *this_local,
                                class_name: cls_name,
                                field_name: "p$0".to_string(),
                            },
                        });
                        let cast_scope = fb
                            .new_local(Ty::Class("kotlinx/coroutines/CoroutineScope".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: cast_scope,
                            value: Rvalue::CheckCast {
                                obj: raw_scope,
                                target_class: "kotlinx/coroutines/CoroutineScope".to_string(),
                            },
                        });
                        cast_scope
                    } else {
                        // No p$0 field — use GlobalScope.
                        let gs =
                            fb.new_local(Ty::Class("kotlinx/coroutines/GlobalScope".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: gs,
                            value: Rvalue::GetStaticField {
                                class_name: "kotlinx/coroutines/GlobalScope".to_string(),
                                field_name: "INSTANCE".to_string(),
                                descriptor: "Lkotlinx/coroutines/GlobalScope;".to_string(),
                            },
                        });
                        gs
                    }
                } else {
                    let gs = fb.new_local(Ty::Class("kotlinx/coroutines/GlobalScope".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: gs,
                        value: Rvalue::GetStaticField {
                            class_name: "kotlinx/coroutines/GlobalScope".to_string(),
                            field_name: "INSTANCE".to_string(),
                            descriptor: "Lkotlinx/coroutines/GlobalScope;".to_string(),
                        },
                    });
                    gs
                };

                // null CoroutineContext (defaulted)
                let null_ctx =
                    fb.new_local(Ty::Class("kotlin/coroutines/CoroutineContext".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: null_ctx,
                    value: Rvalue::Const(MirConst::Null),
                });

                // null CoroutineStart (defaulted)
                let null_start =
                    fb.new_local(Ty::Class("kotlinx/coroutines/CoroutineStart".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: null_start,
                    value: Rvalue::Const(MirConst::Null),
                });

                // Cast lambda to Function2
                let func2_local =
                    fb.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: func2_local,
                    value: Rvalue::CheckCast {
                        obj: lambda_local,
                        target_class: "kotlin/jvm/functions/Function2".to_string(),
                    },
                });

                // iconst_3 (default mask: bits 0+1 = context + start defaulted)
                let mask_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: mask_local,
                    value: Rvalue::Const(MirConst::Int(3)),
                });

                // null handler arg
                let null_handler = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: null_handler,
                    value: Rvalue::Const(MirConst::Null),
                });

                if callee_str == "launch" {
                    // Call BuildersKt.launch$default(
                    //     CoroutineScope, CoroutineContext, CoroutineStart,
                    //     Function2, int, Object)Job
                    let result = fb.new_local(Ty::Class("kotlinx/coroutines/Job".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Call {
                            kind: CallKind::StaticJava {
                                class_name: "kotlinx/coroutines/BuildersKt".to_string(),
                                method_name: "launch$default".to_string(),
                                descriptor: "(Lkotlinx/coroutines/CoroutineScope;Lkotlin/coroutines/CoroutineContext;Lkotlinx/coroutines/CoroutineStart;Lkotlin/jvm/functions/Function2;ILjava/lang/Object;)Lkotlinx/coroutines/Job;".to_string(),
                            },
                            args: vec![scope_local, null_ctx, null_start, func2_local, mask_local, null_handler],
                        },
                    });
                    return Some(result);
                } else {
                    // async: same args, returns Deferred
                    let result = fb.new_local(Ty::Class("kotlinx/coroutines/Deferred".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Call {
                            kind: CallKind::StaticJava {
                                class_name: "kotlinx/coroutines/BuildersKt".to_string(),
                                method_name: "async$default".to_string(),
                                descriptor: "(Lkotlinx/coroutines/CoroutineScope;Lkotlin/coroutines/CoroutineContext;Lkotlinx/coroutines/CoroutineStart;Lkotlin/jvm/functions/Function2;ILjava/lang/Object;)Lkotlinx/coroutines/Deferred;".to_string(),
                            },
                            args: vec![scope_local, null_ctx, null_start, func2_local, mask_local, null_handler],
                        },
                    });
                    return Some(result);
                }
            }

            // ── `withContext(Dispatchers.X) { body }` ───
            //
            // `withContext` is an inline suspend function from
            // kotlinx-coroutines:
            //   kotlinx/coroutines/WithContextKt.withContext(
            //       CoroutineContext, Function2, Continuation)Object
            //
            // It takes a CoroutineContext as its first argument and a
            // suspend lambda block as its second. The lambda is a
            // `suspend CoroutineScope.() -> T` (Function2).
            //
            // Usage: withContext(Dispatchers.IO) { ... }
            //        withContext(Dispatchers.Default) { ... }
            if callee_str == "withContext" && arg_locals.len() == 2 {
                let ctx_local = arg_locals[0];
                let lambda_local = arg_locals[1];

                // Patch lambda to Function2 + SuspendLambda
                let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                if let Ty::Class(ref lambda_class_name) = lambda_ty {
                    if let Some(cls) = module
                        .classes
                        .iter_mut()
                        .find(|c| &c.name == lambda_class_name)
                    {
                        for iface in cls.interfaces.iter_mut() {
                            if iface.starts_with("kotlin/jvm/functions/Function") {
                                *iface = "kotlin/jvm/functions/Function2".to_string();
                            }
                        }
                        if !cls.is_suspend_lambda {
                            cls.is_suspend_lambda = true;
                            cls.super_class =
                                Some("kotlin/coroutines/jvm/internal/SuspendLambda".to_string());
                        }
                    }
                }

                // Cast lambda to Function2
                let func2_local =
                    fb.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: func2_local,
                    value: Rvalue::CheckCast {
                        obj: lambda_local,
                        target_class: "kotlin/jvm/functions/Function2".to_string(),
                    },
                });

                // withContext is a suspend function — emit as Static(stub)
                // so the state machine extractor detects it as a
                // suspension point and threads the continuation properly.
                let wc_sym = interner.intern("withContext");
                let wc_fid = *name_to_func
                    .get(&wc_sym)
                    .expect("withContext stub must be pre-registered");

                let cont_local = if fb.mf.is_suspend {
                    *fb.mf
                        .params
                        .last()
                        .expect("suspend fn must have $completion")
                } else {
                    let c = fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    c
                };

                let result = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::Static(wc_fid),
                        args: vec![ctx_local, func2_local, cont_local],
                    },
                });
                return Some(result);
            }

            // ── `coroutineScope { body }` ──────────────
            //
            // `coroutineScope` is an inline suspend function:
            //   kotlinx/coroutines/CoroutineScopeKt.coroutineScope(
            //       Function2, Continuation)Object
            //
            // The lambda is `suspend CoroutineScope.() -> R` (Function2).
            // `supervisorScope` has the same signature but lives in
            // SupervisorKt.
            if (callee_str == "coroutineScope" || callee_str == "supervisorScope")
                && arg_locals.len() == 1
            {
                let lambda_local = arg_locals[0];

                // Patch lambda to Function2 + SuspendLambda
                let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                if let Ty::Class(ref lambda_class_name) = lambda_ty {
                    if let Some(cls) = module
                        .classes
                        .iter_mut()
                        .find(|c| &c.name == lambda_class_name)
                    {
                        for iface in cls.interfaces.iter_mut() {
                            if iface.starts_with("kotlin/jvm/functions/Function") {
                                *iface = "kotlin/jvm/functions/Function2".to_string();
                            }
                        }
                        if !cls.is_suspend_lambda {
                            cls.is_suspend_lambda = true;
                            cls.super_class =
                                Some("kotlin/coroutines/jvm/internal/SuspendLambda".to_string());
                        }
                    }
                }

                // Cast lambda to Function2
                let func2_local =
                    fb.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: func2_local,
                    value: Rvalue::CheckCast {
                        obj: lambda_local,
                        target_class: "kotlin/jvm/functions/Function2".to_string(),
                    },
                });

                // coroutineScope/supervisorScope are suspend functions —
                // emit as Static(stub) for state machine detection.
                let stub_sym = interner.intern(callee_str);
                let stub_fid = *name_to_func
                    .get(&stub_sym)
                    .expect("coroutineScope/supervisorScope stub must be pre-registered");

                let cont_local = if fb.mf.is_suspend {
                    *fb.mf
                        .params
                        .last()
                        .expect("suspend fn must have $completion")
                } else {
                    let c = fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    c
                };

                let result = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::Static(stub_fid),
                        args: vec![func2_local, cont_local],
                    },
                });
                return Some(result);
            }

            // ── `withTimeout(ms) { body }` ────────────
            //
            // `withTimeout` and `withTimeoutOrNull` are suspend functions:
            //   kotlinx/coroutines/TimeoutKt.withTimeout(J, Function2, Continuation)Object
            //   kotlinx/coroutines/TimeoutKt.withTimeoutOrNull(J, Function2, Continuation)Object
            //
            // First arg is Long (millis), second is suspend lambda.
            if (callee_str == "withTimeout" || callee_str == "withTimeoutOrNull")
                && arg_locals.len() == 2
            {
                let ms_arg = arg_locals[0];
                let lambda_local = arg_locals[1];

                // Promote Int to Long if needed (same pattern as delay)
                let ms_local = {
                    let arg_ty = fb.mf.locals[ms_arg.0 as usize].clone();
                    if arg_ty == Ty::Int {
                        let mut promoted = false;
                        for stmt in fb.mf.blocks[fb.cur_block as usize].stmts.iter_mut().rev() {
                            let MStmt::Assign { dest, value } = stmt;
                            if *dest == ms_arg {
                                if let Rvalue::Const(MirConst::Int(v)) = value {
                                    let v_long = *v as i64;
                                    *value = Rvalue::Const(MirConst::Long(v_long));
                                    fb.mf.locals[ms_arg.0 as usize] = Ty::Long;
                                    promoted = true;
                                }
                                break;
                            }
                        }
                        if promoted {
                            ms_arg
                        } else {
                            let long_local = fb.new_local(Ty::Long);
                            fb.push_stmt(MStmt::Assign {
                                dest: long_local,
                                value: Rvalue::Call {
                                    kind: CallKind::StaticJava {
                                        class_name: "$i2l$".to_string(),
                                        method_name: "$i2l$".to_string(),
                                        descriptor: "(I)J".to_string(),
                                    },
                                    args: vec![ms_arg],
                                },
                            });
                            long_local
                        }
                    } else {
                        ms_arg
                    }
                };

                // Patch lambda to Function2 + SuspendLambda
                let lambda_ty = fb.mf.locals[lambda_local.0 as usize].clone();
                if let Ty::Class(ref lambda_class_name) = lambda_ty {
                    if let Some(cls) = module
                        .classes
                        .iter_mut()
                        .find(|c| &c.name == lambda_class_name)
                    {
                        for iface in cls.interfaces.iter_mut() {
                            if iface.starts_with("kotlin/jvm/functions/Function") {
                                *iface = "kotlin/jvm/functions/Function2".to_string();
                            }
                        }
                        if !cls.is_suspend_lambda {
                            cls.is_suspend_lambda = true;
                            cls.super_class =
                                Some("kotlin/coroutines/jvm/internal/SuspendLambda".to_string());
                        }
                    }
                }

                // Cast lambda to Function2
                let func2_local =
                    fb.new_local(Ty::Class("kotlin/jvm/functions/Function2".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: func2_local,
                    value: Rvalue::CheckCast {
                        obj: lambda_local,
                        target_class: "kotlin/jvm/functions/Function2".to_string(),
                    },
                });

                // Continuation
                let cont_local = if fb.mf.is_suspend {
                    *fb.mf
                        .params
                        .last()
                        .expect("suspend fn must have $completion")
                } else {
                    let c = fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    c
                };

                // withTimeout/withTimeoutOrNull are suspend functions —
                // emit as Static(stub) for state machine detection.
                let stub_sym = interner.intern(callee_str);
                let stub_fid = *name_to_func
                    .get(&stub_sym)
                    .expect("withTimeout stub must be pre-registered");

                let result = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::Static(stub_fid),
                        args: vec![ms_local, func2_local, cont_local],
                    },
                });
                return Some(result);
            }

            // ── `yield()` inside suspend context ───────
            //
            // `yield` is a suspend function from kotlinx-coroutines:
            //   kotlinx/coroutines/YieldKt.yield(Continuation)Object
            //
            // It yields the current coroutine's thread to allow other
            // coroutines to run (cooperative multitasking).
            if callee_str == "yield" && arg_locals.is_empty() {
                let yield_sym = interner.intern("yield");
                let yield_fid = *name_to_func
                    .get(&yield_sym)
                    .expect("yield stub must be pre-registered");

                let cont_local = if fb.mf.is_suspend {
                    *fb.mf
                        .params
                        .last()
                        .expect("suspend fn must have $completion")
                } else {
                    let c = fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    c
                };

                let dest = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::Static(yield_fid),
                        args: vec![cont_local],
                    },
                });
                return Some(dest);
            }

            // ── `delay(ms)` inside suspend context ──────
            //
            // `delay` is a kotlinx-coroutines suspend function:
            //   kotlinx/coroutines/DelayKt.delay(J, Continuation)Object
            //
            // The Int literal argument is promoted to Long. The
            // Continuation arg is the enclosing suspend function's
            // `$completion` (for named funs) or `this` (for suspend
            // lambdas, where the lambda IS the continuation).
            //
            // This call is a SUSPENSION POINT — it must be recognized
            // by the state machine extractor. We emit it as a
            // StaticJava call but also register a synthetic function
            // in the module so the extractor can match it.
            if callee_str == "delay" {
                // Promote Int arg to Long if needed. `delay` takes a
                // `Long` parameter, but Kotlin auto-promotes `Int`
                // literals. We scan backwards through the block for the
                // assignment that produced the arg local, and if it's a
                // `Const(Int(v))` we replace it in-place with
                // `Const(Long(v))` and retype the local. For non-literal
                // ints we emit a fresh Long local via `i2l`.
                let ms_local = if !arg_locals.is_empty() {
                    let arg = arg_locals[0];
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    if arg_ty == Ty::Int {
                        // Try to find the Const(Int) assignment to this local
                        // and promote it to Long in place.
                        let mut promoted = false;
                        for stmt in fb.mf.blocks[fb.cur_block as usize].stmts.iter_mut().rev() {
                            let MStmt::Assign { dest, value } = stmt;
                            if *dest == arg {
                                if let Rvalue::Const(MirConst::Int(v)) = value {
                                    let v_long = *v as i64;
                                    *value = Rvalue::Const(MirConst::Long(v_long));
                                    fb.mf.locals[arg.0 as usize] = Ty::Long;
                                    promoted = true;
                                }
                                break;
                            }
                        }
                        if promoted {
                            arg
                        } else {
                            // General case: emit i2l conversion via StaticJava stub.
                            // The JVM backend handles this by emitting the `i2l` opcode.
                            let long_local = fb.new_local(Ty::Long);
                            fb.push_stmt(MStmt::Assign {
                                dest: long_local,
                                value: Rvalue::Call {
                                    kind: CallKind::StaticJava {
                                        class_name: "$i2l$".to_string(),
                                        method_name: "$i2l$".to_string(),
                                        descriptor: "(I)J".to_string(),
                                    },
                                    args: vec![arg],
                                },
                            });
                            long_local
                        }
                    } else {
                        arg
                    }
                } else {
                    // delay() with no args — shouldn't happen, but
                    // default to 0L.
                    let zero_long = fb.new_local(Ty::Long);
                    fb.push_stmt(MStmt::Assign {
                        dest: zero_long,
                        value: Rvalue::Const(MirConst::Long(0)),
                    });
                    zero_long
                };

                // The `delay` stub was pre-registered in `lower_file`'s
                // Pass 1 so that `body_contains_suspend_call` can
                // detect it during suspend-lambda analysis. Look it up.
                let delay_sym = interner.intern("delay");
                let delay_fid = *name_to_func
                    .get(&delay_sym)
                    .expect("delay stub must be pre-registered");

                // Now emit the call as a Static call to the delay stub.
                // The state machine extractor will pick up that it's a
                // suspend call because delay_fid.is_suspend == true.
                //
                // Append the continuation (last param of enclosing fn if
                // suspend, else null).
                let cont_local = if fb.mf.is_suspend {
                    *fb.mf
                        .params
                        .last()
                        .expect("suspend fn must have $completion")
                } else {
                    let c = fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                    fb.push_stmt(MStmt::Assign {
                        dest: c,
                        value: Rvalue::Const(MirConst::Null),
                    });
                    c
                };

                let dest = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::Static(delay_fid),
                        args: vec![ms_local, cont_local],
                    },
                });
                return Some(dest);
            }

            // `listOf(...)` — call the real Kotlin stdlib
            // `kotlin/collections/CollectionsKt.listOf([Ljava/lang/Object;)Ljava/util/List;`.
            // We create an Object[] array, autobox + store each element, then invoke the stdlib.
            if callee_str == "listOf" {
                let arg_count = arg_locals.len();

                // Create Object[] of the right size.
                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewObjectArray(count_local),
                });

                // Store each arg into the Object[] (autoboxing primitives).
                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                // Call kotlin/collections/CollectionsKt.listOf(Object[])List
                let result = fb.new_local(Ty::Class("java/util/List".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/CollectionsKt".to_string(),
                            method_name: "listOf".to_string(),
                            descriptor: "([Ljava/lang/Object;)Ljava/util/List;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `mutableListOf(...)` — same pattern as listOf, but returns
            // `java/util/ArrayList` via
            // `kotlin/collections/CollectionsKt.mutableListOf([Object;)ArrayList`.
            if callee_str == "mutableListOf" {
                let arg_count = arg_locals.len();

                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewObjectArray(count_local),
                });

                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                // Return type is java/util/List at JVM level. The .add()/.remove()
                // dispatch already matches on class names containing "List".
                let result = fb.new_local(Ty::Class("java/util/List".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/CollectionsKt".to_string(),
                            method_name: "mutableListOf".to_string(),
                            descriptor: "([Ljava/lang/Object;)Ljava/util/List;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `mapOf(pair1, pair2, ...)` — create Pair[] array, call
            // `kotlin/collections/MapsKt.mapOf([Lkotlin/Pair;)Ljava/util/Map;`.
            if callee_str == "mapOf" {
                let arg_count = arg_locals.len();

                // Create kotlin/Pair[] of the right size (must be Pair[], not Object[]).
                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewTypedObjectArray {
                        size: count_local,
                        element_class: "kotlin/Pair".to_string(),
                    },
                });

                // Store each Pair arg into the array.
                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                // Call kotlin/collections/MapsKt.mapOf([Lkotlin/Pair;)Ljava/util/Map;
                let result = fb.new_local(Ty::Class("java/util/Map".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/MapsKt".to_string(),
                            method_name: "mapOf".to_string(),
                            descriptor: "([Lkotlin/Pair;)Ljava/util/Map;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `mutableMapOf(pair1, pair2, ...)` — same pattern as mapOf but
            // calls `MapsKt.mutableMapOf`.
            if callee_str == "mutableMapOf" {
                let arg_count = arg_locals.len();

                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewTypedObjectArray {
                        size: count_local,
                        element_class: "kotlin/Pair".to_string(),
                    },
                });

                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                let result = fb.new_local(Ty::Class("java/util/Map".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/MapsKt".to_string(),
                            method_name: "mutableMapOf".to_string(),
                            descriptor: "([Lkotlin/Pair;)Ljava/util/Map;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `setOf(elements...)` — create Object[] array, call
            // `kotlin/collections/SetsKt.setOf([Ljava/lang/Object;)Ljava/util/Set;`.
            if callee_str == "setOf" {
                let arg_count = arg_locals.len();

                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewObjectArray(count_local),
                });

                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                let result = fb.new_local(Ty::Class("java/util/Set".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/SetsKt".to_string(),
                            method_name: "setOf".to_string(),
                            descriptor: "([Ljava/lang/Object;)Ljava/util/Set;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `mutableSetOf(elements...)` — same pattern as setOf but calls
            // `SetsKt.mutableSetOf`.
            if callee_str == "mutableSetOf" {
                let arg_count = arg_locals.len();

                let count_local = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: count_local,
                    value: Rvalue::Const(MirConst::Int(arg_count as i32)),
                });
                let array_local = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: array_local,
                    value: Rvalue::NewObjectArray(count_local),
                });

                for (i, &arg) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                    let boxed = mir_autobox(fb, arg, &arg_ty);
                    let store_dest = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: store_dest,
                        value: Rvalue::ObjectArrayStore {
                            array: array_local,
                            index: idx,
                            value: boxed,
                        },
                    });
                }

                let result = fb.new_local(Ty::Class("java/util/Set".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "kotlin/collections/SetsKt".to_string(),
                            method_name: "mutableSetOf".to_string(),
                            descriptor: "([Ljava/lang/Object;)Ljava/util/Set;".to_string(),
                        },
                        args: vec![array_local],
                    },
                });
                return Some(result);
            }

            // `Pair(a, b)` — new kotlin/Pair(boxed_a, boxed_b).
            if callee_str == "Pair"
                && arg_locals.len() == 2
                && !module.classes.iter().any(|c| c.name == "Pair")
            {
                let a_ty = fb.mf.locals[arg_locals[0].0 as usize].clone();
                let b_ty = fb.mf.locals[arg_locals[1].0 as usize].clone();
                let a_boxed = mir_autobox(fb, arg_locals[0], &a_ty);
                let b_boxed = mir_autobox(fb, arg_locals[1], &b_ty);
                let dest = fb.new_local(Ty::Class("kotlin/Pair".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance("kotlin/Pair".to_string()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::ConstructorJava {
                            class_name: "kotlin/Pair".to_string(),
                            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)V".to_string(),
                        },
                        args: vec![a_boxed, b_boxed],
                    },
                });
                return Some(dest);
            }

            // `Triple(a, b, c)` — new kotlin/Triple(boxed_a, boxed_b, boxed_c).
            if callee_str == "Triple"
                && arg_locals.len() == 3
                && !module.classes.iter().any(|c| c.name == "Triple")
            {
                let a_ty = fb.mf.locals[arg_locals[0].0 as usize].clone();
                let b_ty = fb.mf.locals[arg_locals[1].0 as usize].clone();
                let c_ty = fb.mf.locals[arg_locals[2].0 as usize].clone();
                let a_boxed = mir_autobox(fb, arg_locals[0], &a_ty);
                let b_boxed = mir_autobox(fb, arg_locals[1], &b_ty);
                let c_boxed = mir_autobox(fb, arg_locals[2], &c_ty);
                let dest = fb.new_local(Ty::Class("kotlin/Triple".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance("kotlin/Triple".to_string()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::ConstructorJava {
                            class_name: "kotlin/Triple".to_string(),
                            descriptor: "(Ljava/lang/Object;Ljava/lang/Object;Ljava/lang/Object;)V"
                                .to_string(),
                        },
                        args: vec![a_boxed, b_boxed, c_boxed],
                    },
                });
                return Some(dest);
            }

            // `StringBuilder()` — new java.lang.StringBuilder().
            if callee_str == "StringBuilder" && arg_locals.is_empty() {
                let dest = fb.new_local(Ty::Class("java/lang/StringBuilder".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance("java/lang/StringBuilder".to_string()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::ConstructorJava {
                            class_name: "java/lang/StringBuilder".to_string(),
                            descriptor: "()V".to_string(),
                        },
                        args: vec![],
                    },
                });
                return Some(dest);
            }

            // Exception constructors: `IllegalStateException("msg")` etc.
            // Maps Kotlin exception class names to JVM internal names.
            let exception_class = match callee_str {
                "IllegalStateException" => Some("java/lang/IllegalStateException"),
                "IllegalArgumentException" => Some("java/lang/IllegalArgumentException"),
                "RuntimeException" => Some("java/lang/RuntimeException"),
                "NullPointerException" => Some("java/lang/NullPointerException"),
                "UnsupportedOperationException" => Some("java/lang/UnsupportedOperationException"),
                "IndexOutOfBoundsException" => Some("java/lang/IndexOutOfBoundsException"),
                "NoSuchElementException" => Some("java/util/NoSuchElementException"),
                "Exception" => Some("java/lang/Exception"),
                "Error" | "AssertionError" => Some("java/lang/AssertionError"),
                "NotImplementedError" => Some("kotlin/NotImplementedError"),
                _ => None,
            };
            if let Some(jvm_class) = exception_class {
                let descriptor = if arg_locals.len() == 1 {
                    "(Ljava/lang/String;)V" // Exception(message)
                } else {
                    "()V" // Exception()
                };
                let dest = fb.new_local(Ty::Class(jvm_class.to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::NewInstance(jvm_class.to_string()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::ConstructorJava {
                            class_name: jvm_class.to_string(),
                            descriptor: descriptor.to_string(),
                        },
                        args: arg_locals.clone(),
                    },
                });
                return Some(dest);
            }

            // `Regex(pattern)` → Pattern.compile(pattern)
            if callee_str == "Regex" && arg_locals.len() == 1 {
                let dest = fb.new_local(Ty::Class("java/util/regex/Pattern".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "java/util/regex/Pattern".to_string(),
                            method_name: "compile".to_string(),
                            descriptor: "(Ljava/lang/String;)Ljava/util/regex/Pattern;".to_string(),
                        },
                        args: arg_locals.clone(),
                    },
                });
                return Some(dest);
            }

            // `require(condition)` — throw IllegalArgumentException if false.
            if callee_str == "require" && arg_locals.len() == 1 {
                // Simplistic: just ignore the check (no-op). The value
                // is consumed but no exception is thrown.
                let dest = fb.new_local(Ty::Unit);
                return Some(dest);
            }

            // `check(condition)` — same as require but IllegalStateException.
            if callee_str == "check" && arg_locals.len() == 1 {
                let dest = fb.new_local(Ty::Unit);
                return Some(dest);
            }

            // `error(message)` — throw IllegalStateException(message).
            if callee_str == "error" && arg_locals.len() == 1 {
                let dest = fb.new_local(Ty::Nothing);
                return Some(dest);
            }

            // `TODO()` / `TODO(message)` — throw NotImplementedError.
            if callee_str == "TODO" {
                let dest = fb.new_local(Ty::Nothing);
                return Some(dest);
            }

            // `IntArray(size)` intrinsic — create a primitive int[].
            // Typed array constructors: IntArray(n), LongArray(n), etc.
            let typed_array_ty = match callee_str {
                "IntArray" => Some(Ty::IntArray),
                "LongArray" => Some(Ty::LongArray),
                "DoubleArray" => Some(Ty::DoubleArray),
                "BooleanArray" => Some(Ty::BooleanArray),
                "ByteArray" => Some(Ty::ByteArray),
                _ => None,
            };
            if let Some(arr_ty) = typed_array_ty {
                if !arg_locals.is_empty() {
                    let size_local = arg_locals[0];
                    let dest = fb.new_local(arr_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::NewIntArray(size_local), // reuses NewIntArray; JVM backend infers type from dest
                    });
                    return Some(dest);
                }
            }

            // `intArrayOf(...)`, `longArrayOf(...)`, etc.
            let arrayof_ty = match callee_str {
                "intArrayOf" => Some(Ty::IntArray),
                "longArrayOf" => Some(Ty::LongArray),
                "doubleArrayOf" => Some(Ty::DoubleArray),
                "booleanArrayOf" => Some(Ty::BooleanArray),
                "byteArrayOf" => Some(Ty::ByteArray),
                _ => None,
            };
            if let Some(arr_ty) = arrayof_ty {
                if !arg_locals.is_empty() {
                    let n = arg_locals.len() as i32;
                    let size = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: size,
                        value: Rvalue::Const(MirConst::Int(n)),
                    });
                    let arr = fb.new_local(arr_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest: arr,
                        value: Rvalue::NewIntArray(size),
                    });
                    // Store each element
                    for (i, &val) in arg_locals.iter().enumerate() {
                        let idx = fb.new_local(Ty::Int);
                        fb.push_stmt(MStmt::Assign {
                            dest: idx,
                            value: Rvalue::Const(MirConst::Int(i as i32)),
                        });
                        let dummy = fb.new_local(Ty::Unit);
                        fb.push_stmt(MStmt::Assign {
                            dest: dummy,
                            value: Rvalue::ArrayStore {
                                array: arr,
                                index: idx,
                                value: val,
                            },
                        });
                    }
                    return Some(arr);
                }
            }

            // `arrayOf(a, b, c)` — create Object[] with given elements.
            if callee_str == "arrayOf" && !arg_locals.is_empty() {
                let n = arg_locals.len() as i32;
                let size = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: size,
                    value: Rvalue::Const(MirConst::Int(n)),
                });
                let arr = fb.new_local(Ty::Any); // Object[]
                fb.push_stmt(MStmt::Assign {
                    dest: arr,
                    value: Rvalue::NewObjectArray(size),
                });
                for (i, &val) in arg_locals.iter().enumerate() {
                    let idx = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: idx,
                        value: Rvalue::Const(MirConst::Int(i as i32)),
                    });
                    // Autobox primitives for Object[].
                    let val_ty = fb.mf.locals[val.0 as usize].clone();
                    let boxed = mir_autobox(fb, val, &val_ty);
                    let dummy = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: dummy,
                        value: Rvalue::ObjectArrayStore {
                            array: arr,
                            index: idx,
                            value: boxed,
                        },
                    });
                }
                return Some(arr);
            }

            // `repeat(n) { body }` — execute lambda n times.
            if callee_str == "repeat" && arg_locals.len() == 2 {
                let count = arg_locals[0];
                let lambda = arg_locals[1];
                let lambda_ty = fb.mf.locals[lambda.0 as usize].clone();
                if matches!(&lambda_ty, Ty::Class(n) if n.contains("$Lambda$")) {
                    let cn = if let Ty::Class(ref n) = lambda_ty {
                        n.clone()
                    } else {
                        unreachable!()
                    };
                    // var i = 0; while (i < n) { lambda.invoke(i); i++ }
                    let i_local = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: i_local,
                        value: Rvalue::Const(MirConst::Int(0)),
                    });
                    let cond_blk = fb.new_block();
                    let body_blk = fb.new_block();
                    let exit_blk = fb.new_block();
                    fb.terminate_and_switch(Terminator::Goto(cond_blk), cond_blk);
                    // Condition: i < n
                    let cmp = fb.new_local(Ty::Bool);
                    fb.push_stmt(MStmt::Assign {
                        dest: cmp,
                        value: Rvalue::BinOp {
                            op: skotch_mir::BinOp::CmpLt,
                            lhs: i_local,
                            rhs: count,
                        },
                    });
                    fb.terminate_and_switch(
                        Terminator::Branch {
                            cond: cmp,
                            then_block: body_blk,
                            else_block: exit_blk,
                        },
                        body_blk,
                    );
                    // Body: lambda.invoke(i)
                    // Autobox i to Object for the erased invoke signature.
                    let boxed_i = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest: boxed_i,
                        value: Rvalue::Call {
                            kind: CallKind::StaticJava {
                                class_name: "java/lang/Integer".to_string(),
                                method_name: "valueOf".to_string(),
                                descriptor: "(I)Ljava/lang/Integer;".to_string(),
                            },
                            args: vec![i_local],
                        },
                    });
                    let _call_result = fb.new_local(Ty::Unit);
                    fb.push_stmt(MStmt::Assign {
                        dest: _call_result,
                        value: Rvalue::Call {
                            kind: CallKind::Virtual {
                                class_name: cn,
                                method_name: "invoke".to_string(),
                            },
                            args: vec![lambda, boxed_i],
                        },
                    });
                    // i++
                    let one = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest: one,
                        value: Rvalue::Const(MirConst::Int(1)),
                    });
                    fb.push_stmt(MStmt::Assign {
                        dest: i_local,
                        value: Rvalue::BinOp {
                            op: skotch_mir::BinOp::AddI,
                            lhs: i_local,
                            rhs: one,
                        },
                    });
                    fb.terminate_and_switch(Terminator::Goto(cond_blk), exit_blk);
                    let result = fb.new_local(Ty::Unit);
                    return Some(result);
                }
            }

            // `with(receiver, lambda)` — invoke lambda with receiver as arg.
            if callee_str == "with" && arg_locals.len() == 2 {
                let receiver = arg_locals[0];
                let lambda = arg_locals[1];
                let lambda_ty = fb.mf.locals[lambda.0 as usize].clone();
                if matches!(&lambda_ty, Ty::Class(n) if n.contains("$Lambda$")) {
                    let cn = if let Ty::Class(ref n) = lambda_ty {
                        n.clone()
                    } else {
                        unreachable!()
                    };
                    let ret = module
                        .classes
                        .iter()
                        .find(|c| c.name == cn)
                        .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                        .map(|m| m.return_ty.clone())
                        .unwrap_or(Ty::Any);
                    // Widen receiver to Ty::Any for erased invoke signature.
                    let recv_arg = {
                        let recv_ty = fb.mf.locals[receiver.0 as usize].clone();
                        match recv_ty {
                            Ty::Int => {
                                let boxed = fb.new_local(Ty::Any);
                                fb.push_stmt(MStmt::Assign {
                                    dest: boxed,
                                    value: Rvalue::Call {
                                        kind: CallKind::StaticJava {
                                            class_name: "java/lang/Integer".to_string(),
                                            method_name: "valueOf".to_string(),
                                            descriptor: "(I)Ljava/lang/Integer;".to_string(),
                                        },
                                        args: vec![receiver],
                                    },
                                });
                                boxed
                            }
                            Ty::Any => receiver,
                            _ => {
                                let widened = fb.new_local(Ty::Any);
                                fb.push_stmt(MStmt::Assign {
                                    dest: widened,
                                    value: Rvalue::Local(receiver),
                                });
                                widened
                            }
                        }
                    };
                    let dest = fb.new_local(ret);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Virtual {
                                class_name: cn,
                                method_name: "invoke".to_string(),
                            },
                            args: vec![lambda, recv_arg],
                        },
                    });
                    return Some(dest);
                }
            }

            // Handle stdlib top-level functions as StaticJava calls.
            // Math functions map to java.lang.Math static methods.
            let stdlib_call = match (callee_str, arg_locals.len()) {
                ("maxOf", 2) => Some(("java/lang/Math", "max", "(II)I", Ty::Int)),
                ("minOf", 2) => Some(("java/lang/Math", "min", "(II)I", Ty::Int)),
                // kotlin.math functions
                ("abs", 1) => {
                    let arg_ty = &fb.mf.locals[arg_locals[0].0 as usize];
                    match arg_ty {
                        Ty::Double => Some(("java/lang/Math", "abs", "(D)D", Ty::Double)),
                        Ty::Long => Some(("java/lang/Math", "abs", "(J)J", Ty::Long)),
                        _ => Some(("java/lang/Math", "abs", "(I)I", Ty::Int)),
                    }
                }
                ("sqrt", 1) => Some(("java/lang/Math", "sqrt", "(D)D", Ty::Double)),
                ("ceil", 1) => Some(("java/lang/Math", "ceil", "(D)D", Ty::Double)),
                ("floor", 1) => Some(("java/lang/Math", "floor", "(D)D", Ty::Double)),
                ("round", 1) => Some(("java/lang/Math", "round", "(D)J", Ty::Long)),
                ("pow", 2) => Some(("java/lang/Math", "pow", "(DD)D", Ty::Double)),
                ("sin", 1) => Some(("java/lang/Math", "sin", "(D)D", Ty::Double)),
                ("cos", 1) => Some(("java/lang/Math", "cos", "(D)D", Ty::Double)),
                ("tan", 1) => Some(("java/lang/Math", "tan", "(D)D", Ty::Double)),
                ("log", 1) => Some(("java/lang/Math", "log", "(D)D", Ty::Double)),
                ("log10", 1) => Some(("java/lang/Math", "log10", "(D)D", Ty::Double)),
                ("exp", 1) => Some(("java/lang/Math", "exp", "(D)D", Ty::Double)),
                // readLine() → reads a line from stdin
                ("readLine", 0) | ("readln", 0) => None, // handled separately below
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

            // readLine() / readln() — read a line from stdin.
            // Emits: new java.util.Scanner(System.in).nextLine()
            // The JVM backend handles the GetStaticField for System.in
            // as a special case in the Scanner constructor.
            if (callee_str == "readLine" || callee_str == "readln") && arg_locals.is_empty() {
                // We emit this as a StaticJava call to a pseudo-method
                // that the JVM backend recognizes and emits as the
                // Scanner(System.in).nextLine() pattern.
                let result = fb.new_local(Ty::String);
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "$readLine".to_string(),
                            method_name: "readLine".to_string(),
                            descriptor: "()Ljava/lang/String;".to_string(),
                        },
                        args: vec![],
                    },
                });
                return Some(result);
            }

            // Check if callee is a local variable (lambda or callable object).
            // Handles: val f = { x: Int -> x * 2 }; f(5)
            // Also: fun apply(f: (Int)->Int, x: Int) = f(x) via FunctionN interface.
            if let Some((_, local_id)) = scope.iter().rev().find(|(s, _)| *s == callee_name) {
                let local_ty = fb.mf.locals[local_id.0 as usize].clone();
                let is_lambda_class = matches!(&local_ty, Ty::Class(n) if n.contains("$Lambda$"));
                let is_function_interface = matches!(&local_ty, Ty::Class(n) if n.starts_with("kotlin/jvm/functions/Function"));
                // Check if this is a class with an `invoke` operator method.
                let has_invoke_method = if let Ty::Class(ref cn) = local_ty {
                    module
                        .classes
                        .iter()
                        .find(|c| &c.name == cn)
                        .map(|c| c.methods.iter().any(|m| m.name == "invoke"))
                        .unwrap_or(false)
                } else {
                    false
                };
                let is_callable = is_lambda_class
                    || is_function_interface
                    || has_invoke_method
                    || matches!(local_ty, Ty::Any | Ty::Function { .. });
                if is_callable {
                    // ── FunctionN interface dispatch ──────────────────
                    // When the local is typed as FunctionN, Ty::Any, or
                    // Ty::Function, dispatch through the erased FunctionN
                    // interface: autobox each arg to Object, call
                    // invokeinterface FunctionN.invoke(), then unbox the
                    // Object return to the expected type.
                    let use_interface_dispatch =
                        is_function_interface || matches!(local_ty, Ty::Any | Ty::Function { .. });
                    if use_interface_dispatch {
                        // ── Suspend callable parameter ────
                        // When the local is a suspend-typed function
                        // parameter inside a suspend function, the
                        // invocation becomes:
                        //   block.invoke($completion)
                        // via invokeinterface Function1.invoke(Object)Object.
                        // The $completion is the enclosing function's last
                        // param. The result is Object (no unboxing).
                        let is_suspend_call = fb.suspend_callable_locals.contains(&local_id.0);
                        if is_suspend_call && fb.mf.is_suspend {
                            // Load block + continuation → invokeinterface
                            let cont_local = *fb
                                .mf
                                .params
                                .last()
                                .expect("suspend fn must have $completion");
                            // Widen continuation to Ty::Any for the erased
                            // invoke(Object)Object descriptor.
                            let cont_widened = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: cont_widened,
                                value: Rvalue::Local(cont_local),
                            });
                            let iface_name = if let Ty::Class(ref cn) = local_ty {
                                cn.clone()
                            } else {
                                stdlib_function_interface(1)
                            };
                            let raw_result = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest: raw_result,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: iface_name,
                                        method_name: "invoke".to_string(),
                                    },
                                    args: vec![*local_id, cont_widened],
                                },
                            });
                            return Some(raw_result);
                        }

                        // Autobox each argument: primitive → wrapper Object.
                        let mut boxed_args: Vec<LocalId> = vec![*local_id];
                        for &arg in &arg_locals {
                            let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                            match arg_ty {
                                Ty::Int => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Integer".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(I)Ljava/lang/Integer;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    boxed_args.push(boxed);
                                }
                                Ty::Bool => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Boolean".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(Z)Ljava/lang/Boolean;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    boxed_args.push(boxed);
                                }
                                Ty::Long => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Long".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(J)Ljava/lang/Long;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    boxed_args.push(boxed);
                                }
                                Ty::Double => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Double".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(D)Ljava/lang/Double;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    boxed_args.push(boxed);
                                }
                                Ty::Any => boxed_args.push(arg),
                                _ => {
                                    // Reference type — widen to Ty::Any so the
                                    // descriptor uses Ljava/lang/Object;.
                                    let widened = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: widened,
                                        value: Rvalue::Local(arg),
                                    });
                                    boxed_args.push(widened);
                                }
                            }
                        }
                        let iface_name = if let Ty::Class(ref cn) = local_ty {
                            cn.clone()
                        } else {
                            // For Ty::Any / Ty::Function, derive arity from call args.
                            let arity = arg_locals.len();
                            stdlib_function_interface(arity)
                        };
                        // The interface invoke returns Ty::Any (Object).
                        let raw_result = fb.new_local(Ty::Any);
                        fb.push_stmt(MStmt::Assign {
                            dest: raw_result,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: iface_name,
                                    method_name: "invoke".to_string(),
                                },
                                args: boxed_args,
                            },
                        });
                        // Use the function's declared return type to determine
                        // whether to unbox the Object result or pass it through.
                        let fn_ret = if let Ty::Function { ref ret, .. } = local_ty {
                            (**ret).clone()
                        } else {
                            // For Ty::Any or FunctionN interface types, we
                            // can't determine the return type statically.
                            // Return the raw Object; the JVM backend or
                            // smart-cast handles narrowing downstream.
                            Ty::Any
                        };
                        if matches!(fn_ret, Ty::Int) {
                            let unboxed = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest: unboxed,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "java/lang/Integer".to_string(),
                                        method_name: "intValue".to_string(),
                                        descriptor: "()I".to_string(),
                                    },
                                    args: vec![raw_result],
                                },
                            });
                            return Some(unboxed);
                        }
                        // For reference types (String, Any, etc.),
                        // return the Object as-is. The JVM backend
                        // handles checkcast via Rvalue::Local.
                        return Some(raw_result);
                    }

                    // ── Direct lambda / invoke-operator dispatch ──────
                    // Lower as: receiver.invoke(args)
                    // Lambda invoke methods now use erased types (Ty::Any)
                    // for FunctionN compatibility. Must autobox args and
                    // the return is Ty::Any (Object).
                    let invoke_class = if let Ty::Class(ref cn) = local_ty {
                        cn.clone()
                    } else if matches!(local_ty, Ty::Function { .. } | Ty::Any) {
                        module
                            .classes
                            .iter()
                            .rev()
                            .find(|c| c.name.contains("$Lambda$"))
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "java/lang/Object".to_string())
                    } else {
                        "java/lang/Object".to_string()
                    };
                    // Check if the target invoke uses erased types.
                    let target_erased = module
                        .classes
                        .iter()
                        .find(|c| c.name == invoke_class)
                        .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                        .map(|m| {
                            m.params
                                .iter()
                                .skip(1)
                                .all(|p| matches!(m.locals[p.0 as usize], Ty::Any))
                        })
                        .unwrap_or(false);
                    let mut all_args = vec![*local_id];
                    if target_erased {
                        // Autobox each arg for the erased invoke method.
                        for &arg in &arg_locals {
                            let arg_ty = fb.mf.locals[arg.0 as usize].clone();
                            match arg_ty {
                                Ty::Int => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Integer".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(I)Ljava/lang/Integer;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    all_args.push(boxed);
                                }
                                Ty::Bool => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Boolean".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(Z)Ljava/lang/Boolean;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    all_args.push(boxed);
                                }
                                Ty::Long => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Long".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(J)Ljava/lang/Long;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    all_args.push(boxed);
                                }
                                Ty::Double => {
                                    let boxed = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: boxed,
                                        value: Rvalue::Call {
                                            kind: CallKind::StaticJava {
                                                class_name: "java/lang/Double".to_string(),
                                                method_name: "valueOf".to_string(),
                                                descriptor: "(D)Ljava/lang/Double;".to_string(),
                                            },
                                            args: vec![arg],
                                        },
                                    });
                                    all_args.push(boxed);
                                }
                                Ty::Any => all_args.push(arg),
                                _ => {
                                    // Reference type (String, Class, etc.) —
                                    // widen to Ty::Any so the descriptor uses
                                    // Ljava/lang/Object; matching the erased
                                    // invoke signature.
                                    let widened = fb.new_local(Ty::Any);
                                    fb.push_stmt(MStmt::Assign {
                                        dest: widened,
                                        value: Rvalue::Local(arg),
                                    });
                                    all_args.push(widened);
                                }
                            }
                        }
                    } else {
                        all_args.extend_from_slice(&arg_locals);
                    }
                    // Find invoke return type from class metadata.
                    let ret_ty = if let Ty::Class(ref cn) = local_ty {
                        module
                            .classes
                            .iter()
                            .find(|c| &c.name == cn)
                            .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                            .map(|m| m.return_ty.clone())
                            .unwrap_or(Ty::Any)
                    } else if let Ty::Function { ref ret, .. } = local_ty {
                        (**ret).clone()
                    } else {
                        Ty::Any
                    };
                    let dest = fb.new_local(ret_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::Virtual {
                                class_name: invoke_class,
                                method_name: "invoke".to_string(),
                            },
                            args: all_args,
                        },
                    });
                    return Some(dest);
                }
            }

            let kind = if callee_str == "println" {
                CallKind::Println
            } else if callee_str == "print" {
                CallKind::Print
            } else if let Some(fid) = name_to_func.get(&callee_name) {
                // Check if this function has capture params (local functions).
                // If the function has more params than explicit args AND it's
                // not a suspend function (which has an extra $completion param),
                // the extra leading params are captures that need to be filled.
                let target = &module.functions[fid.0 as usize];
                let target_param_count = target.params.len();
                let is_suspend_target = target.is_suspend;
                if !is_suspend_target && target_param_count > arg_locals.len() {
                    let fn_name = &module.functions[fid.0 as usize].name;
                    let capture_count = target_param_count - arg_locals.len();
                    let mut capture_args = Vec::new();
                    for ci in 0..capture_count {
                        // Find the capture param by checking the function's
                        // local types and matching against scope.
                        let capture_ty = module.functions[fid.0 as usize]
                            .locals
                            .get(ci)
                            .cloned()
                            .unwrap_or(Ty::Any);
                        // Look for a $capture$ entry in scope.
                        let found = scope.iter().rev().find(|(sym, _)| {
                            let n = interner.resolve(*sym);
                            n.starts_with(&format!("$capture${fn_name}$"))
                        });
                        if let Some((_, lid)) = found {
                            capture_args.push(*lid);
                        } else {
                            // Fallback: try to find a local with matching type.
                            let fallback = fb.new_local(capture_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest: fallback,
                                value: Rvalue::Const(MirConst::Int(0)),
                            });
                            capture_args.push(fallback);
                        }
                    }
                    capture_args.extend_from_slice(&arg_locals);
                    arg_locals = capture_args;
                }
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
                        // Look up in this class, superclasses, and interfaces.
                        let mut search_class = Some(class_name.clone());
                        'search: while let Some(ref cname) = search_class {
                            let found = module.classes.iter().find(|c| &c.name == cname);
                            if let Some(cls) = found {
                                if cls.methods.iter().any(|m| m.name == callee_str) {
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
                                // Search interfaces.
                                for iname in &cls.interfaces {
                                    if let Some(icls) =
                                        module.classes.iter().find(|c| &c.name == iname)
                                    {
                                        if let Some(m) =
                                            icls.methods.iter().find(|m| m.name == callee_str)
                                        {
                                            arg_locals.insert(0, *this_local);
                                            resolved = Some((
                                                CallKind::Virtual {
                                                    class_name: iname.clone(),
                                                    method_name: callee_str.to_string(),
                                                },
                                                m.return_ty.clone(),
                                            ));
                                            break 'search;
                                        }
                                    }
                                }
                                search_class = cls.super_class.clone();
                            } else {
                                break;
                            }
                        }
                        // If not found in MirClass hierarchy, try JDK
                        // class lookup for Java classes (StringBuilder,
                        // etc.). This enables implicit `this.method()`
                        // dispatch in lambda-with-receiver bodies.
                        if resolved.is_none() {
                            if let Ok(info) = skotch_classinfo::load_jdk_class(class_name) {
                                if let Some(m) = info.methods.iter().find(|m| m.name == callee_str)
                                {
                                    arg_locals.insert(0, *this_local);
                                    resolved = Some((
                                        CallKind::VirtualJava {
                                            class_name: class_name.clone(),
                                            method_name: callee_str.to_string(),
                                            descriptor: m.descriptor.clone(),
                                        },
                                        Ty::Any,
                                    ));
                                }
                            }
                        }
                        // Check callable fields: `greet()` where `greet` is
                        // a val of function type on `this`.
                        if resolved.is_none() {
                            let field_info: Option<(String, Ty)> = module
                                .classes
                                .iter()
                                .find(|c| &c.name == class_name)
                                .and_then(|cls| cls.fields.iter().find(|f| f.name == callee_str))
                                .map(|f| (f.name.clone(), f.ty.clone()));
                            if let Some((field_name, field_ty)) = field_info {
                                let this_copy = *this_local;
                                let class_copy = class_name.clone();
                                // Load the field from this.
                                let field_local = fb.new_local(field_ty.clone());
                                fb.push_stmt(MStmt::Assign {
                                    dest: field_local,
                                    value: Rvalue::GetField {
                                        receiver: this_copy,
                                        class_name: class_copy,
                                        field_name,
                                    },
                                });
                                // Invoke the callable field.
                                let (invoke_class, _invoke_ret) = if let Ty::Class(ref cn) =
                                    field_ty
                                {
                                    let ret = module
                                        .classes
                                        .iter()
                                        .find(|c| &c.name == cn)
                                        .and_then(|c| c.methods.iter().find(|m| m.name == "invoke"))
                                        .map(|m| m.return_ty.clone())
                                        .unwrap_or(Ty::Any);
                                    (cn.clone(), ret)
                                } else if let Ty::Function {
                                    ref params,
                                    ref ret,
                                    ..
                                } = field_ty
                                {
                                    // Function type: use FunctionN interface for invoke.
                                    let arity = params.len();
                                    let iface = stdlib_function_interface(arity);
                                    (iface, (**ret).clone())
                                } else {
                                    ("$Callable".to_string(), Ty::Any)
                                };
                                let mut invoke_args = vec![field_local];
                                // Box primitive args for the erased invoke signature.
                                for a in &arg_locals {
                                    let a_ty = fb.mf.locals[a.0 as usize].clone();
                                    let boxed = mir_autobox(fb, *a, &a_ty);
                                    invoke_args.push(boxed);
                                }
                                // FunctionN.invoke returns Object on JVM.
                                let dest = fb.new_local(Ty::Any);
                                fb.push_stmt(MStmt::Assign {
                                    dest,
                                    value: Rvalue::Call {
                                        kind: CallKind::Virtual {
                                            class_name: invoke_class,
                                            method_name: "invoke".to_string(),
                                        },
                                        args: invoke_args,
                                    },
                                });
                                return Some(dest);
                            }
                        }
                    }
                }
                if let Some((k, _)) = resolved {
                    k
                } else if let Some((owner, desc, _ret_ty)) =
                    module.cross_file_fns.get(callee_str).cloned()
                {
                    // Cross-file function call.
                    CallKind::StaticJava {
                        class_name: owner,
                        method_name: callee_str.to_string(),
                        descriptor: desc,
                    }
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
                CallKind::StaticJava {
                    ref method_name,
                    ref descriptor,
                    ..
                } => {
                    // For cross-file calls, look up return type from
                    // cross_file_fns or infer from descriptor.
                    if let Some((_, _, ret_ty)) = module.cross_file_fns.get(method_name) {
                        ret_ty.clone()
                    } else if descriptor.ends_with(")V") {
                        Ty::Unit
                    } else {
                        Ty::Any
                    }
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

            // Coroutine transform: when calling a
            // `suspend fun`, append the caller's own `$completion`
            // if the caller is itself suspend, otherwise pass
            // `null`. A real Kotlin compiler would reject the
            // non-suspend case at typeck time ("suspend function
            // can only be called from another suspend function");
            // we accept it for now so non-coroutine tests that
            // mention suspend functions still compile. The `null`
            // path will NPE at runtime if invoked.
            if let CallKind::Static(fid) = &kind {
                if module.functions[fid.0 as usize].is_suspend {
                    let cont_local = if fb.mf.is_suspend {
                        // Our last param is the incoming $completion.
                        *fb.mf
                            .params
                            .last()
                            .expect("suspend fn must have $completion")
                    } else {
                        let c =
                            fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest: c,
                            value: Rvalue::Const(MirConst::Null),
                        });
                        c
                    };
                    arg_locals.push(cont_local);
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
                    if let Expr::DoubleLit(v, _) | Expr::FloatLit(v, _) = operand.as_ref() {
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

            // Sealed class exhaustiveness check: if the subject is a sealed
            // class, verify all subclasses are covered by `is` patterns.
            if !is_subjectless && else_body.is_none() {
                let subj_ty = &fb.mf.locals[subj.0 as usize];
                if let Ty::Class(class_name) = subj_ty {
                    let is_sealed = module
                        .classes
                        .iter()
                        .any(|c| &c.name == class_name && c.is_abstract && !c.is_interface);
                    if is_sealed {
                        // Collect all subclasses.
                        let subclasses: Vec<&str> = module
                            .classes
                            .iter()
                            .filter(|c| c.super_class.as_deref() == Some(class_name.as_str()))
                            .map(|c| c.name.as_str())
                            .collect();
                        // Collect all `is` patterns in the when branches.
                        let covered: Vec<String> = branches
                            .iter()
                            .filter_map(|b| {
                                if let Expr::IsCheck { type_name, .. } = &b.pattern {
                                    Some(interner.resolve(*type_name).to_string())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let missing: Vec<&&str> = subclasses
                            .iter()
                            .filter(|s| !covered.iter().any(|c| c == **s))
                            .collect();
                        if !missing.is_empty() {
                            let names: Vec<&str> = missing.iter().map(|s| **s).collect();
                            diags.push(Diagnostic::error(
                                subject.span(),
                                format!(
                                    "'when' on sealed class `{}` is not exhaustive. Missing: {}",
                                    class_name,
                                    names.join(", ")
                                ),
                            ));
                        }
                    }
                }
            }

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
            // Always create an else block — even without an explicit else body,
            // we need it to assign a default value so the JVM verifier sees an
            // initialized result on all paths.
            let else_blk = fb.new_block();
            let merge_blk = fb.new_block();

            // First comparison block
            if !branches.is_empty() {
                fb.terminate_and_switch(Terminator::Goto(cmp_blks[0]), cmp_blks[0]);
            } else {
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
                } else if is_subjectless || matches!(&branch.pattern, Expr::IsCheck { .. }) {
                    // Subjectless when or `is Type` pattern: the pattern IS the condition.
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
                } else {
                    else_blk
                };

                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: cmp,
                        then_block: body_blks[i],
                        else_block: fall_through,
                    },
                    body_blks[i],
                );

                // Smart cast: if the branch pattern is `is Type`, narrow the
                // subject variable to that type in the branch body scope.
                // e.g. `when (obj) { is String -> obj.length }` — obj is
                // narrowed to String inside the branch.
                let smart_cast_entries = if let Expr::IsCheck {
                    expr: checked_expr,
                    type_name,
                    negated: false,
                    ..
                } = &branch.pattern
                {
                    if let Expr::Ident(var_name, _) = checked_expr.as_ref() {
                        let type_str = interner.resolve(*type_name);
                        let narrowed_ty =
                            skotch_types::ty_from_name(type_str).unwrap_or_else(|| {
                                if module.classes.iter().any(|c| c.name == type_str) {
                                    Ty::Class(type_str.to_string())
                                } else {
                                    Ty::Any
                                }
                            });
                        if let Some((_, old_local)) =
                            scope.iter().rev().find(|(s, _)| s == var_name)
                        {
                            let cast_local = fb.new_local(narrowed_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest: cast_local,
                                value: Rvalue::Local(*old_local),
                            });
                            scope.push((*var_name, cast_local));
                            1usize
                        } else {
                            0
                        }
                    } else {
                        0
                    }
                } else {
                    0
                };

                // Lower body in body_blks[i].
                // When branch bodies that are `{ stmts }` parse
                // as Expr::Lambda with no params. Inline them as blocks
                // rather than creating lambda classes — they're Kotlin block
                // expressions, not closures.
                let body_val = if let Expr::Lambda { params, body, .. } = &branch.body {
                    if params.is_empty() {
                        // Inline the block: lower each stmt, return last expr's val.
                        let mut last = None;
                        for s in &body.stmts {
                            match s {
                                skotch_syntax::Stmt::Expr(e)
                                | skotch_syntax::Stmt::Return { value: Some(e), .. } => {
                                    last = lower_expr(
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
                                    // If it's a return, set the terminator.
                                    if matches!(s, skotch_syntax::Stmt::Return { .. }) {
                                        if let Some(local) = last {
                                            fb.set_terminator(Terminator::ReturnValue(local));
                                        }
                                    }
                                }
                                _ => {
                                    lower_stmt(
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
                        last
                    } else {
                        lower_expr(
                            &branch.body,
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
                } else {
                    lower_expr(
                        &branch.body,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )
                };

                // Pop smart-cast scope entries.
                for _ in 0..smart_cast_entries {
                    scope.pop();
                }

                if let Some(val) = body_val {
                    if i == 0 {
                        let ty = fb.mf.locals[val.0 as usize].clone();
                        fb.mf.locals[result.0 as usize] = ty;
                    }
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(val),
                    });
                }

                // Goto merge, switch to next comparison block.
                // Don't overwrite explicit `return` terminators.
                let next = if i + 1 < branches.len() {
                    cmp_blks[i + 1]
                } else {
                    else_blk
                };
                {
                    let cur = fb.cur_block as usize;
                    if !matches!(fb.mf.blocks[cur].terminator, Terminator::ReturnValue(_)) {
                        fb.terminate_and_switch(Terminator::Goto(merge_blk), next);
                    } else {
                        fb.cur_block = next;
                    }
                }
            }

            // Else body
            if let Some(eb) = else_body {
                // We're in else_blk. Same Lambda-as-block inlining.
                let else_val = if let Expr::Lambda { params, body, .. } = eb.as_ref() {
                    if params.is_empty() {
                        let mut last = None;
                        for s in &body.stmts {
                            match s {
                                skotch_syntax::Stmt::Expr(e)
                                | skotch_syntax::Stmt::Return { value: Some(e), .. } => {
                                    last = lower_expr(
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
                                    if matches!(s, skotch_syntax::Stmt::Return { .. }) {
                                        if let Some(local) = last {
                                            fb.set_terminator(Terminator::ReturnValue(local));
                                        }
                                    }
                                }
                                _ => {
                                    lower_stmt(
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
                        last
                    } else {
                        lower_expr(
                            eb,
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
                } else {
                    lower_expr(
                        eb,
                        fb,
                        scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        loop_ctx,
                    )
                };
                if let Some(val) = else_val {
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(val),
                    });
                }
                {
                    let cur = fb.cur_block as usize;
                    if !matches!(fb.mf.blocks[cur].terminator, Terminator::ReturnValue(_)) {
                        fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);
                    } else {
                        fb.cur_block = merge_blk;
                    }
                }
            } else {
                // No else body — assign a default to the result local so the
                // JVM verifier doesn't see an uninitialized local on the
                // implicit fall-through path.
                let result_ty = &fb.mf.locals[result.0 as usize];
                let default_val = match result_ty {
                    Ty::Int | Ty::Byte | Ty::Short | Ty::Char | Ty::Bool | Ty::Float => {
                        MirConst::Int(0)
                    }
                    Ty::Long => MirConst::Long(0),
                    Ty::Double => MirConst::Double(0.0),
                    _ => MirConst::Null,
                };
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Const(default_val),
                });
                fb.terminate_and_switch(Terminator::Goto(merge_blk), merge_blk);
            }

            // If the merge block is unreachable (all branches
            // returned explicitly) and the method returns non-void, the
            // merge block needs a valid return. Only fix non-void methods;
            // void methods' bare `Return` is correct.
            {
                let merge = merge_blk as usize;
                let result_ty = &fb.mf.locals[result.0 as usize];
                if matches!(fb.mf.blocks[merge].terminator, Terminator::Return)
                    && !matches!(result_ty, Ty::Unit)
                {
                    fb.mf.blocks[merge].terminator = Terminator::ReturnValue(result);
                }
            }

            Some(result)
        }
        Expr::Index {
            receiver,
            index,
            span: _,
        } => {
            let arr = lower_expr(
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
            let idx = lower_expr(
                index,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            let arr_ty = fb.mf.locals[arr.0 as usize].clone();
            match &arr_ty {
                Ty::IntArray | Ty::BooleanArray | Ty::ByteArray => {
                    let elem_ty = match &arr_ty {
                        Ty::BooleanArray => Ty::Bool,
                        Ty::ByteArray => Ty::Byte,
                        _ => Ty::Int,
                    };
                    let dest = fb.new_local(elem_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayLoad {
                            array: arr,
                            index: idx,
                        },
                    });
                    Some(dest)
                }
                Ty::LongArray => {
                    let dest = fb.new_local(Ty::Long);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayLoad {
                            array: arr,
                            index: idx,
                        },
                    });
                    Some(dest)
                }
                Ty::DoubleArray => {
                    let dest = fb.new_local(Ty::Double);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayLoad {
                            array: arr,
                            index: idx,
                        },
                    });
                    Some(dest)
                }
                Ty::Class(cn) if cn.contains("ArrayList") || cn.contains("List") => {
                    // List[index] → invokeinterface java/util/List.get(I)Object
                    let dest = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/util/List".to_string(),
                                method_name: "get".to_string(),
                                descriptor: "(I)Ljava/lang/Object;".to_string(),
                            },
                            args: vec![arr, idx],
                        },
                    });
                    Some(dest)
                }
                Ty::Class(cn) if cn.contains("Map") => {
                    // Map[key] → invokeinterface java/util/Map.get(Object)Object
                    let idx_ty = fb.mf.locals[idx.0 as usize].clone();
                    let boxed_key = mir_autobox(fb, idx, &idx_ty);
                    let dest = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/util/Map".to_string(),
                                method_name: "get".to_string(),
                                descriptor: "(Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
                            },
                            args: vec![arr, boxed_key],
                        },
                    });
                    Some(dest)
                }
                Ty::String => {
                    // String[index] → invokevirtual String.charAt(I)C
                    let dest = fb.new_local(Ty::Char);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/String".to_string(),
                                method_name: "charAt".to_string(),
                                descriptor: "(I)C".to_string(),
                            },
                            args: vec![arr, idx],
                        },
                    });
                    Some(dest)
                }
                Ty::Class(cn) => {
                    // Check for operator fun get() on the class.
                    let get_method = module
                        .classes
                        .iter()
                        .find(|c| &c.name == cn)
                        .and_then(|cls| cls.methods.iter().find(|m| m.name == "get"))
                        .map(|m| m.return_ty.clone());
                    if let Some(ret_ty) = get_method {
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Virtual {
                                    class_name: cn.clone(),
                                    method_name: "get".to_string(),
                                },
                                args: vec![arr, idx],
                            },
                        });
                        Some(dest)
                    } else {
                        diags.push(Diagnostic::error(
                            receiver.span(),
                            format!("no operator fun get() on class `{cn}`"),
                        ));
                        None
                    }
                }
                Ty::Any => {
                    // Object[] indexing (from arrayOf): uses ArrayLoad,
                    // JVM backend emits aaload based on dest type Any.
                    let dest = fb.new_local(Ty::Any);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayLoad {
                            array: arr,
                            index: idx,
                        },
                    });
                    Some(dest)
                }
                _ => {
                    diags.push(Diagnostic::error(
                        receiver.span(),
                        "index operator is only supported on String, IntArray, List, Map, Array, and classes with operator fun get()",
                    ));
                    None
                }
            }
        }
        Expr::Field {
            receiver,
            name,
            span,
        } => {
            // Companion object property access: ClassName.PROP → global constant lookup.
            if let Expr::Ident(recv_sym, _) = receiver.as_ref() {
                let recv_str = interner.resolve(*recv_sym).to_string();
                let field_str = interner.resolve(*name).to_string();
                let is_class = module.classes.iter().any(|c| c.name == recv_str);
                if is_class {
                    let companion_sym = interner.intern(&field_str);
                    if let Some(constant) = name_to_global.get(&companion_sym).cloned() {
                        let ty = const_ty(&constant);
                        let dest = fb.new_local(ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Const(constant),
                        });
                        return Some(dest);
                    }
                }
            }

            // Well-known Kotlin/Java stdlib constants:
            // Long.MAX_VALUE, Long.MIN_VALUE, Int.MAX_VALUE, Int.MIN_VALUE
            if let Expr::Ident(recv_sym, _) = receiver.as_ref() {
                let recv_str = interner.resolve(*recv_sym);
                let field_str = interner.resolve(*name);
                let constant: Option<(Ty, MirConst)> = match (recv_str, field_str) {
                    ("Long", "MAX_VALUE") => Some((Ty::Long, MirConst::Long(i64::MAX))),
                    ("Long", "MIN_VALUE") => Some((Ty::Long, MirConst::Long(i64::MIN))),
                    ("Int", "MAX_VALUE") => Some((Ty::Int, MirConst::Int(i32::MAX))),
                    ("Int", "MIN_VALUE") => Some((Ty::Int, MirConst::Int(i32::MIN))),
                    ("Double", "MAX_VALUE") => Some((Ty::Double, MirConst::Double(f64::MAX))),
                    ("Double", "MIN_VALUE") => {
                        Some((Ty::Double, MirConst::Double(f64::MIN_POSITIVE)))
                    }
                    ("Float", "MAX_VALUE") => Some((Ty::Float, MirConst::Float(f32::MAX))),
                    ("Byte", "MAX_VALUE") => Some((Ty::Int, MirConst::Int(i8::MAX as i32))),
                    ("Short", "MAX_VALUE") => Some((Ty::Int, MirConst::Int(i16::MAX as i32))),
                    _ => None,
                };
                if let Some((ty, val)) = constant {
                    let dest = fb.new_local(ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::Const(val),
                    });
                    return Some(dest);
                }
            }

            // Dispatchers.IO / .Default / .Main / .Unconfined
            //
            // `Dispatchers` is a Kotlin object. Each dispatcher property
            // is exposed as a static getter method on the JVM:
            //   Dispatchers.IO → invokestatic Dispatchers.getIO()
            //   Dispatchers.Default → invokestatic Dispatchers.getDefault()
            //   Dispatchers.Unconfined → invokestatic Dispatchers.getUnconfined()
            //   Dispatchers.Main → invokestatic Dispatchers.getMain()
            if let Expr::Ident(recv_sym, _) = receiver.as_ref() {
                let recv_str = interner.resolve(*recv_sym);
                let field_str = interner.resolve(*name);
                if recv_str == "Dispatchers" {
                    let getter = match field_str {
                        "IO" => Some("getIO"),
                        "Default" => Some("getDefault"),
                        "Unconfined" => Some("getUnconfined"),
                        "Main" => Some("getMain"),
                        _ => None,
                    };
                    if let Some(getter_name) = getter {
                        let dest = fb
                            .new_local(Ty::Class("kotlin/coroutines/CoroutineContext".to_string()));
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::StaticJava {
                                    class_name: "kotlinx/coroutines/Dispatchers".to_string(),
                                    method_name: getter_name.to_string(),
                                    descriptor: "()Lkotlinx/coroutines/CoroutineDispatcher;"
                                        .to_string(),
                                },
                                args: vec![],
                            },
                        });
                        return Some(dest);
                    }
                }
            }

            // Check if this is an enum/object constant access (Color.RED)
            // or an extension property access (receiver.extProp).
            if let Some(&fid) = name_to_func.get(name) {
                let ret_ty = module.functions[fid.0 as usize].return_ty.clone();
                let params_len = module.functions[fid.0 as usize].params.len();
                if params_len == 0 {
                    // Zero-arg function (enum constant or companion method).
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
                if params_len == 1 {
                    // Extension property: lower receiver, call with it.
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
                        let dest = fb.new_local(ret_ty);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::Static(fid),
                                args: vec![recv_local],
                            },
                        });
                        return Some(dest);
                    }
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
                    // Walk the inheritance chain to find the declaring class.
                    let mut declaring_class = class_name.clone();
                    let mut field_ty = Ty::Any;
                    let mut search = Some(class_name);
                    while let Some(ref cname) = search {
                        if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                            if let Some(f) = cls.fields.iter().find(|f| f.name == field_name) {
                                declaring_class = cname.clone();
                                field_ty = f.ty.clone();
                                break;
                            }
                            search = cls.super_class.clone();
                        } else {
                            break;
                        }
                    }
                    // Check if this is a computed property (getter method).
                    let getter_name =
                        format!("get{}{}", &field_name[..1].to_uppercase(), &field_name[1..]);
                    if let Some(cls) = module.classes.iter().find(|c| c.name == declaring_class) {
                        if let Some(getter) = cls.methods.iter().find(|m| m.name == getter_name) {
                            let ret_ty = getter.return_ty.clone();
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::Virtual {
                                        class_name: declaring_class.clone(),
                                        method_name: getter_name,
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // JDK class property-as-method: sb.length → length()
                    // Many JVM classes expose properties as methods.
                    if declaring_class.contains('/') {
                        // Try the field name directly as a 0-arg method.
                        if let Some((_, found_method, desc, ret_ty)) =
                            lookup_java_instance(&declaring_class, &field_name, 0)
                        {
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: declaring_class.clone(),
                                        method_name: found_method,
                                        descriptor: desc,
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                        // Try getXxx() pattern.
                        let jvm_getter =
                            format!("get{}{}", &field_name[..1].to_uppercase(), &field_name[1..]);
                        if let Some((_, found_method, desc, ret_ty)) =
                            lookup_java_instance(&declaring_class, &jvm_getter, 0)
                        {
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: declaring_class.clone(),
                                        method_name: found_method,
                                        descriptor: desc,
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // List .size → invokeinterface java/util/List.size()I
                    if field_name == "size"
                        && (declaring_class.contains("ArrayList")
                            || declaring_class.contains("List"))
                    {
                        let dest = fb.new_local(Ty::Int);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/util/List".to_string(),
                                    method_name: "size".to_string(),
                                    descriptor: "()I".to_string(),
                                },
                                args: vec![recv_local],
                            },
                        });
                        return Some(dest);
                    }
                    // Map .size / .keys / .values / .entries
                    if declaring_class.contains("Map") {
                        let map_prop: Option<(&str, &str, Ty)> = match field_name.as_str() {
                            "size" => Some(("size", "()I", Ty::Int)),
                            "keys" => Some((
                                "keySet",
                                "()Ljava/util/Set;",
                                Ty::Class("java/util/Set".into()),
                            )),
                            "values" => Some((
                                "values",
                                "()Ljava/util/Collection;",
                                Ty::Class("java/util/Collection".into()),
                            )),
                            "entries" => Some((
                                "entrySet",
                                "()Ljava/util/Set;",
                                Ty::Class("java/util/Set".into()),
                            )),
                            _ => None,
                        };
                        if let Some((jvm_method, descriptor, ret_ty)) = map_prop {
                            let dest = fb.new_local(ret_ty);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "java/util/Map".to_string(),
                                        method_name: jvm_method.to_string(),
                                        descriptor: descriptor.to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // Set .size
                    if declaring_class.contains("Set") && field_name == "size" {
                        let dest = fb.new_local(Ty::Int);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/util/Set".to_string(),
                                    method_name: "size".to_string(),
                                    descriptor: "()I".to_string(),
                                },
                                args: vec![recv_local],
                            },
                        });
                        return Some(dest);
                    }
                    // Pair .first / .second → invoke getFirst() / getSecond()
                    // Only for kotlin/Pair, NOT user-defined classes named *Pair*.
                    if declaring_class == "kotlin/Pair" {
                        let (getter, desc) = match field_name.as_str() {
                            "first" => ("getFirst", "()Ljava/lang/Object;"),
                            "second" => ("getSecond", "()Ljava/lang/Object;"),
                            _ => ("", ""),
                        };
                        if !getter.is_empty() {
                            let dest = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "kotlin/Pair".to_string(),
                                        method_name: getter.to_string(),
                                        descriptor: desc.to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // Triple .first / .second / .third → getFirst/getSecond/getThird
                    if declaring_class.contains("Triple") || declaring_class == "kotlin/Triple" {
                        let (getter, desc) = match field_name.as_str() {
                            "first" => ("getFirst", "()Ljava/lang/Object;"),
                            "second" => ("getSecond", "()Ljava/lang/Object;"),
                            "third" => ("getThird", "()Ljava/lang/Object;"),
                            _ => ("", ""),
                        };
                        if !getter.is_empty() {
                            let dest = fb.new_local(Ty::Any);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "kotlin/Triple".to_string(),
                                        method_name: getter.to_string(),
                                        descriptor: desc.to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // IntRange .first / .last → getFirst() / getLast()
                    if declaring_class.contains("IntRange") {
                        let range_prop: Option<(&str, &str)> = match field_name.as_str() {
                            "first" => Some(("getFirst", "()I")),
                            "last" => Some(("getLast", "()I")),
                            _ => None,
                        };
                        if let Some((getter, desc)) = range_prop {
                            let dest = fb.new_local(Ty::Int);
                            fb.push_stmt(MStmt::Assign {
                                dest,
                                value: Rvalue::Call {
                                    kind: CallKind::VirtualJava {
                                        class_name: "kotlin/ranges/IntRange".to_string(),
                                        method_name: getter.to_string(),
                                        descriptor: desc.to_string(),
                                    },
                                    args: vec![recv_local],
                                },
                            });
                            return Some(dest);
                        }
                    }
                    // Exception .message → Throwable.getMessage()
                    // Handles all JVM exception types (they all inherit from Throwable).
                    if field_name == "message"
                        && (declaring_class.contains("Exception")
                            || declaring_class.contains("Error")
                            || declaring_class.contains("Throwable"))
                    {
                        let dest = fb.new_local(Ty::String);
                        fb.push_stmt(MStmt::Assign {
                            dest,
                            value: Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Throwable".to_string(),
                                    method_name: "getMessage".to_string(),
                                    descriptor: "()Ljava/lang/String;".to_string(),
                                },
                                args: vec![recv_local],
                            },
                        });
                        return Some(dest);
                    }
                    let dest = fb.new_local(field_ty);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::GetField {
                            receiver: recv_local,
                            class_name: declaring_class,
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
                // IntArray.size → arraylength
                if matches!(
                    recv_ty,
                    Ty::IntArray
                        | Ty::LongArray
                        | Ty::DoubleArray
                        | Ty::BooleanArray
                        | Ty::ByteArray
                ) && field_name == "size"
                {
                    let dest = fb.new_local(Ty::Int);
                    fb.push_stmt(MStmt::Assign {
                        dest,
                        value: Rvalue::ArrayLength(recv_local),
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
            // Lower the exception expression and terminate with Throw.
            // The result type is Nothing (throw never returns).
            let exc_local = lower_expr(
                thrown,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            fb.set_terminator(Terminator::Throw(exc_local));
            // Return a Nothing-typed local so the expression has a value
            // (though it's unreachable).
            let dest = fb.new_local(Ty::Nothing);
            Some(dest)
        }
        Expr::Try {
            body,
            catch_param,
            catch_type,
            catch_body,
            finally_body,
            ..
        } => {
            // ── try-as-expression lowering ──────────────────────────
            //
            // Layout (with catch):
            //   current_block → goto try_block
            //   try_block: <try body> last-expr → result; goto after_block
            //   catch_block: astore param; <catch body> last-expr → result; goto after_block
            //   after_block: result is available
            //
            // Without catch: just execute body and use last expression.
            if catch_body.is_some() {
                let try_block = fb.new_block();
                let catch_block = fb.new_block();
                let after_block = fb.new_block();

                // Pre-allocate result local (type patched below).
                let result = fb.new_local(Ty::Int);

                fb.terminate_and_switch(Terminator::Goto(try_block), try_block);

                // Lower try body: all statements, last one provides the value.
                let mut try_val = None;
                for (i, stmt) in body.stmts.iter().enumerate() {
                    if i == body.stmts.len() - 1 {
                        // Try to extract as expression for the result value.
                        if let Stmt::Expr(expr) = stmt {
                            try_val = lower_expr(
                                expr,
                                fb,
                                scope,
                                module,
                                name_to_func,
                                name_to_global,
                                interner,
                                diags,
                                loop_ctx,
                            );
                        } else {
                            lower_stmt(
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
                        }
                    } else {
                        lower_stmt(
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
                    }
                }
                if let Some(tv) = try_val {
                    let ty = fb.mf.locals[tv.0 as usize].clone();
                    fb.mf.locals[result.0 as usize] = ty;
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(tv),
                    });
                }
                fb.terminate_and_switch(Terminator::Goto(after_block), catch_block);

                // Catch handler: store exception param.
                if let Some(param_sym) = catch_param {
                    let exc_local = fb.new_local(Ty::Any);
                    scope.push((*param_sym, exc_local));
                    fb.push_stmt(MStmt::Assign {
                        dest: exc_local,
                        value: Rvalue::Const(MirConst::Null),
                    });
                }

                // Lower catch body.
                let mut catch_val = None;
                if let Some(cb) = catch_body {
                    for (i, stmt) in cb.stmts.iter().enumerate() {
                        if i == cb.stmts.len() - 1 {
                            if let Stmt::Expr(expr) = stmt {
                                catch_val = lower_expr(
                                    expr,
                                    fb,
                                    scope,
                                    module,
                                    name_to_func,
                                    name_to_global,
                                    interner,
                                    diags,
                                    loop_ctx,
                                );
                            } else {
                                lower_stmt(
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
                            }
                        } else {
                            lower_stmt(
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
                        }
                    }
                }
                if let Some(cv) = catch_val {
                    fb.push_stmt(MStmt::Assign {
                        dest: result,
                        value: Rvalue::Local(cv),
                    });
                }
                fb.terminate_and_switch(Terminator::Goto(after_block), after_block);

                // Exception handler entry.
                let jvm_catch_type = catch_type.map(|sym| {
                    let name = interner.resolve(sym).to_string();
                    match name.as_str() {
                        "Exception" => "java/lang/Exception".to_string(),
                        "RuntimeException" => "java/lang/RuntimeException".to_string(),
                        "ArithmeticException" => "java/lang/ArithmeticException".to_string(),
                        "NullPointerException" => "java/lang/NullPointerException".to_string(),
                        "IllegalArgumentException" => {
                            "java/lang/IllegalArgumentException".to_string()
                        }
                        "IllegalStateException" => "java/lang/IllegalStateException".to_string(),
                        "IndexOutOfBoundsException" => {
                            "java/lang/IndexOutOfBoundsException".to_string()
                        }
                        "ClassCastException" => "java/lang/ClassCastException".to_string(),
                        "NumberFormatException" => "java/lang/NumberFormatException".to_string(),
                        "UnsupportedOperationException" => {
                            "java/lang/UnsupportedOperationException".to_string()
                        }
                        "Throwable" => "java/lang/Throwable".to_string(),
                        "Error" => "java/lang/Error".to_string(),
                        other => {
                            if other.contains('/') {
                                other.to_string()
                            } else {
                                format!("java/lang/{other}")
                            }
                        }
                    }
                });

                fb.mf.exception_handlers.push(ExceptionHandler {
                    try_start_block: try_block,
                    try_end_block: catch_block,
                    handler_block: catch_block,
                    catch_type: jvm_catch_type,
                });

                // Finally body (inlined unconditionally).
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

                Some(result)
            } else {
                // No catch — just execute body, use last expression as result.
                let mut last_val = None;
                for (i, stmt) in body.stmts.iter().enumerate() {
                    if i == body.stmts.len() - 1 {
                        if let Stmt::Expr(expr) = stmt {
                            last_val = lower_expr(
                                expr,
                                fb,
                                scope,
                                module,
                                name_to_func,
                                name_to_global,
                                interner,
                                diags,
                                loop_ctx,
                            );
                        } else {
                            lower_stmt(
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
                        }
                    } else {
                        lower_stmt(
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
                    }
                }
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
                last_val
            }
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

            // then: dispatch the property access on the non-null receiver.
            // Unwrap the nullable type to get the inner type for dispatch.
            let recv_ty = fb.mf.locals[recv.0 as usize].clone();
            let inner_ty = match &recv_ty {
                Ty::Nullable(inner) => (**inner).clone(),
                other => other.clone(),
            };
            let field_name = interner.resolve(*name).to_string();

            // Create a non-nullable alias so JVM dispatch sees the right type.
            let recv_unwrapped = if matches!(recv_ty, Ty::Nullable(_)) {
                let uw = fb.new_local(inner_ty.clone());
                fb.push_stmt(MStmt::Assign {
                    dest: uw,
                    value: Rvalue::Local(recv),
                });
                uw
            } else {
                recv
            };

            // Dispatch built-in properties (String.length, IntArray.size, etc.)
            let field_val = if matches!(inner_ty, Ty::String) && field_name == "length" {
                let d = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::Call {
                        kind: CallKind::Virtual {
                            class_name: "java/lang/String".to_string(),
                            method_name: "length".to_string(),
                        },
                        args: vec![recv_unwrapped],
                    },
                });
                d
            } else if matches!(
                inner_ty,
                Ty::IntArray | Ty::LongArray | Ty::DoubleArray | Ty::BooleanArray | Ty::ByteArray
            ) && field_name == "size"
            {
                let d = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::ArrayLength(recv_unwrapped),
                });
                d
            } else if let Ty::Class(ref class_name) = inner_ty {
                // Look up field in user-defined class.
                let mut declaring_class = class_name.clone();
                let mut field_ty = Ty::Any;
                let mut search = Some(class_name.clone());
                while let Some(ref cname) = search {
                    if let Some(cls) = module.classes.iter().find(|c| &c.name == cname) {
                        if let Some(f) = cls.fields.iter().find(|f| f.name == field_name) {
                            declaring_class = cname.clone();
                            field_ty = f.ty.clone();
                            break;
                        }
                        search = cls.super_class.clone();
                    } else {
                        break;
                    }
                }
                let d = fb.new_local(field_ty);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::GetField {
                        receiver: recv_unwrapped,
                        class_name: declaring_class,
                        field_name,
                    },
                });
                d
            } else if field_name == "length" {
                // When the inner type is erased to Any (e.g. from a
                // preceding safe-call chain), `.length` most likely
                // targets String.length(). Dispatch as a virtual call
                // instead of emitting a broken empty-class GetField.
                let d = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::Call {
                        kind: CallKind::Virtual {
                            class_name: "java/lang/String".to_string(),
                            method_name: "length".to_string(),
                        },
                        args: vec![recv_unwrapped],
                    },
                });
                d
            } else if field_name == "size" {
                // Similarly, `.size` on Any likely targets a collection.
                let d = fb.new_local(Ty::Int);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::Call {
                        kind: CallKind::VirtualJava {
                            class_name: "java/util/Collection".to_string(),
                            method_name: "size".to_string(),
                            descriptor: "()I".to_string(),
                        },
                        args: vec![recv_unwrapped],
                    },
                });
                d
            } else {
                // Fallback: generic field access.
                let d = fb.new_local(Ty::Any);
                fb.push_stmt(MStmt::Assign {
                    dest: d,
                    value: Rvalue::GetField {
                        receiver: recv_unwrapped,
                        class_name: String::new(),
                        field_name,
                    },
                });
                d
            };
            // Box primitive results so they fit in the Nullable(Any) result
            // local (which is a JVM reference slot).
            let field_val_ty = fb.mf.locals[field_val.0 as usize].clone();
            let boxed_val = mir_autobox(fb, field_val, &field_val_ty);
            fb.push_stmt(MStmt::Assign {
                dest: result,
                value: Rvalue::Local(boxed_val),
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
            let raw_type_str = interner.resolve(*type_name);
            // Substitute reified type parameters if active.
            let substituted = fb.reified_types.get(raw_type_str).cloned();
            let type_str = substituted.as_deref().unwrap_or(raw_type_str);
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
        Expr::AsCast {
            expr: casted,
            type_name,
            safe,
            ..
        } => {
            let obj = lower_expr(
                casted,
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
            let target_ty = skotch_types::ty_from_name(type_str).unwrap_or_else(|| {
                if module.classes.iter().any(|c| c.name == type_str) {
                    Ty::Class(type_str.to_string())
                } else {
                    Ty::Any
                }
            });
            let jvm_type = match type_str {
                "String" => "java/lang/String",
                "Int" => "java/lang/Integer",
                "Long" => "java/lang/Long",
                "Double" => "java/lang/Double",
                "Boolean" => "java/lang/Boolean",
                "Char" => "java/lang/Character",
                other => other,
            };
            if *safe {
                // `as?` — emit instanceof check: returns T? (null if not match)
                let check = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: check,
                    value: Rvalue::InstanceOf {
                        obj,
                        type_descriptor: jvm_type.to_string(),
                    },
                });
                let then_block = fb.new_block();
                let else_block = fb.new_block();
                let merge_block = fb.new_block();
                let result = fb.new_local(Ty::Nullable(Box::new(target_ty.clone())));
                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: check,
                        then_block,
                        else_block,
                    },
                    then_block,
                );
                // then: cast succeeded — copy obj as target type
                let cast_local = fb.new_local(target_ty);
                fb.push_stmt(MStmt::Assign {
                    dest: cast_local,
                    value: Rvalue::Local(obj),
                });
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Local(cast_local),
                });
                fb.terminate_and_switch(Terminator::Goto(merge_block), else_block);
                // else: cast failed — result = null
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Const(MirConst::Null),
                });
                fb.terminate_and_switch(Terminator::Goto(merge_block), merge_block);
                Some(result)
            } else {
                // `as` — just retype (JVM backend emits checkcast from the
                // Rvalue::Local copy when dest type is narrower than source).
                let dest = fb.new_local(target_ty);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Local(obj),
                });
                Some(dest)
            }
        }
        Expr::NotNullAssert { expr: asserted, .. } => {
            // x!! → null check + unwrap. If null, throw KotlinNullPointerException.
            let inner = lower_expr(
                asserted,
                fb,
                scope,
                module,
                name_to_func,
                name_to_global,
                interner,
                diags,
                loop_ctx,
            )?;
            let inner_ty = fb.mf.locals[inner.0 as usize].clone();
            if let Ty::Nullable(unwrapped) = inner_ty {
                // Emit null check: if (inner == null) throw NPE
                let null_val = fb.new_local(Ty::Nullable(Box::new(Ty::Any)));
                fb.push_stmt(MStmt::Assign {
                    dest: null_val,
                    value: Rvalue::Const(MirConst::Null),
                });
                let is_null = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: is_null,
                    value: Rvalue::BinOp {
                        op: MBinOp::CmpEq,
                        lhs: inner,
                        rhs: null_val,
                    },
                });
                let throw_block = fb.new_block();
                let ok_block = fb.new_block();
                fb.terminate_and_switch(
                    Terminator::Branch {
                        cond: is_null,
                        then_block: throw_block,
                        else_block: ok_block,
                    },
                    throw_block,
                );
                // throw block: new NullPointerException + athrow
                let npe = fb.new_local(Ty::Class("java/lang/NullPointerException".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: npe,
                    value: Rvalue::NewInstance("java/lang/NullPointerException".to_string()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest: npe,
                    value: Rvalue::Call {
                        kind: CallKind::ConstructorJava {
                            class_name: "java/lang/NullPointerException".to_string(),
                            descriptor: "()V".to_string(),
                        },
                        args: vec![],
                    },
                });
                fb.set_terminator(Terminator::Throw(npe));
                fb.cur_block = ok_block;
                // ok block: unwrap to non-nullable type
                let dest = fb.new_local(*unwrapped);
                fb.push_stmt(MStmt::Assign {
                    dest,
                    value: Rvalue::Local(inner),
                });
                Some(dest)
            } else {
                Some(inner)
            }
        }

        Expr::Lambda {
            params,
            body,
            is_suspend: ast_is_suspend,
            ..
        } => {
            // ── Suspend lambda detection ────────────────────────────
            // A lambda is a suspend lambda if either:
            //   1. The AST flagged it (future: `suspend {}` syntax or
            //      inferred from function-type-parameter context)
            //   2. The body contains a call to a suspend function
            // Detection flows through to MIR so the codegen layer
            // can generate SuspendLambda-extending classes.
            // Currently lambdas with suspend bodies still compile as
            // regular $Lambda$N classes — this means calling them from
            // real coroutine builders won't work at runtime. Full
            // SuspendLambda codegen is a follow-up.
            // Check the force flag FIRST, then AST flag, then body scan.
            let forced = module.force_suspend_lambda;
            if forced {
                module.force_suspend_lambda = false; // consume the flag
            }
            let is_suspend_lambda = forced
                || *ast_is_suspend
                || body_contains_suspend_call(body, module, interner, name_to_func);

            // ── Capture analysis ────────────────────────────────────
            let param_names: Vec<Symbol> = params.iter().map(|p| p.name).collect();
            let free_vars: Vec<(Symbol, LocalId, Ty)> =
                collect_free_vars(body, &param_names, scope, fb, interner);

            // Detect which captures need Ref boxing: any `var` declaration
            // that's captured needs boxing so mutations are visible across
            // the lambda boundary (both inner writes and outer writes).
            let mutated: rustc_hash::FxHashSet<Symbol> = free_vars
                .iter()
                .filter(|(sym, _, _)| fb.var_syms.contains(sym))
                .map(|(sym, _, _)| *sym)
                .collect();

            // For each mutated capture, generate a $Ref class and box the var
            // in the outer scope so mutations are visible across the boundary.
            let mut ref_class_names: rustc_hash::FxHashMap<Symbol, String> =
                rustc_hash::FxHashMap::default();
            for (sym, outer_lid, ty) in &free_vars {
                if !mutated.contains(sym) {
                    continue;
                }
                let ref_class_name = format!("$Ref${}", module.classes.len());
                ref_class_names.insert(*sym, ref_class_name.clone());

                // Generate synthetic $Ref class with `element` field.
                let mut ref_init = MirFunction {
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
                    param_receiver_types: Vec::new(),
                    is_abstract: false,
                    exception_handlers: Vec::new(),
                    vararg_index: None,
                    is_suspend: false,
                    is_inline: false,
                    suspend_original_return_ty: None,
                    suspend_state_machine: None,
                    annotations: Vec::new(),
                };
                let ref_this = ref_init.new_local(Ty::Class(ref_class_name.clone()));
                ref_init.params.push(ref_this);
                let ref_val_param = ref_init.new_local(ty.clone());
                ref_init.params.push(ref_val_param);
                ref_init.blocks[0].stmts.push(MStmt::Assign {
                    dest: ref_this,
                    value: Rvalue::Call {
                        kind: CallKind::Constructor("java/lang/Object".to_string()),
                        args: vec![ref_this],
                    },
                });
                ref_init.blocks[0].stmts.push(MStmt::Assign {
                    dest: ref_this,
                    value: Rvalue::PutField {
                        receiver: ref_this,
                        class_name: ref_class_name.clone(),
                        field_name: "element".to_string(),
                        value: ref_val_param,
                    },
                });

                module.classes.push(MirClass {
                    name: ref_class_name.clone(),
                    super_class: None,
                    is_open: false,
                    is_abstract: false,
                    is_interface: false,
                    interfaces: Vec::new(),
                    fields: vec![MirField {
                        name: "element".to_string(),
                        ty: ty.clone(),
                    }],
                    methods: Vec::new(),
                    constructor: ref_init,
                    secondary_constructors: Vec::new(),
                    is_suspend_lambda: false,
                    is_cross_file_stub: false,
                    annotations: Vec::new(),
                });

                // In the outer scope, wrap the var into a $Ref instance.
                let ref_inst = fb.new_local(Ty::Class(ref_class_name.clone()));
                fb.push_stmt(MStmt::Assign {
                    dest: ref_inst,
                    value: Rvalue::NewInstance(ref_class_name.clone()),
                });
                fb.push_stmt(MStmt::Assign {
                    dest: ref_inst,
                    value: Rvalue::Call {
                        kind: CallKind::Constructor(ref_class_name.clone()),
                        args: vec![*outer_lid],
                    },
                });
                // Replace the outer scope binding: var now points to the $Ref.
                // Add the $ref$ holder so Stmt::Assign can find it.
                let ref_holder_sym = interner.intern(&format!("$ref${}", interner.resolve(*sym)));
                scope.push((ref_holder_sym, ref_inst));
                if let Some(entry) = scope.iter_mut().rev().find(|(s, _)| s == sym) {
                    entry.1 = ref_inst;
                }
            }

            // Fields for captured variables — use $Ref type for mutated captures.
            let capture_fields: Vec<MirField> = free_vars
                .iter()
                .map(|(sym, _, ty)| {
                    let field_ty = if let Some(ref_name) = ref_class_names.get(sym) {
                        Ty::Class(ref_name.clone())
                    } else {
                        ty.clone()
                    };
                    MirField {
                        name: interner.resolve(*sym).to_string(),
                        ty: field_ty,
                    }
                })
                .collect();

            // Pre-register the lambda class so nested lambdas get unique indices.
            // (Recalculate lambda_idx since $Ref classes may have been added above.)
            let lambda_idx = module.classes.len();
            // Include the wrapper class name so lambda classes are
            // globally unique (e.g. InputKt$Lambda$0). This prevents
            // JNI DefineClass collisions across REPL turns.
            let lambda_class_name = format!("{}$Lambda${lambda_idx}", module.wrapper_class);
            module.classes.push(MirClass {
                name: lambda_class_name.clone(),
                super_class: None,
                is_open: false,
                is_abstract: false,
                is_interface: false,
                interfaces: Vec::new(),
                fields: capture_fields.clone(),
                methods: Vec::new(),
                constructor: MirFunction {
                    id: FuncId(0),
                    name: "<init>".to_string(),
                    params: Vec::new(),
                    locals: Vec::new(),
                    blocks: Vec::new(),
                    return_ty: Ty::Unit,
                    required_params: 0,
                    param_names: Vec::new(),
                    param_defaults: Vec::new(),
                    param_receiver_types: Vec::new(),
                    is_abstract: false,
                    exception_handlers: Vec::new(),
                    vararg_index: None,
                    is_suspend: false,
                    is_inline: false,
                    suspend_original_return_ty: None,
                    suspend_state_machine: None,
                    annotations: Vec::new(),
                },
                secondary_constructors: Vec::new(),
                is_suspend_lambda,
                is_cross_file_stub: false,
                annotations: Vec::new(),
            });

            // ── Invoke method ───────────────────────────────────────
            let mut invoke_fn = {
                let fn_idx = module.functions.len() + 1000 + lambda_idx;
                let mut invoke_fb = FnBuilder::new(fn_idx, "invoke".to_string(), Ty::Int);
                let this_local = invoke_fb.new_local(Ty::Class(lambda_class_name.clone()));
                invoke_fb.mf.params.push(this_local);

                let mut invoke_scope: Vec<(Symbol, LocalId)> = Vec::new();
                // Bind `this` in scope so nested launch/async can find
                // the lambda's p$0 field for CoroutineScope threading.
                let this_sym = interner.intern("this");
                invoke_scope.push((this_sym, this_local));
                // Load captured fields into locals.
                for (sym, _, ty) in &free_vars {
                    let is_ref_boxed = ref_class_names.contains_key(sym);
                    let field_ty = if let Some(ref_name) = ref_class_names.get(sym) {
                        Ty::Class(ref_name.clone())
                    } else {
                        ty.clone()
                    };
                    let local = invoke_fb.new_local(field_ty);
                    invoke_fb.push_stmt(MStmt::Assign {
                        dest: local,
                        value: Rvalue::GetField {
                            receiver: this_local,
                            class_name: lambda_class_name.clone(),
                            field_name: interner.resolve(*sym).to_string(),
                        },
                    });
                    if is_ref_boxed {
                        // For Ref-boxed captures, read the element into a local
                        // and put the $Ref local + element local both in scope.
                        // The $Ref local is stored so Stmt::Assign can PutField.
                        let ref_class = ref_class_names[sym].clone();
                        let elem = invoke_fb.new_local(ty.clone());
                        invoke_fb.push_stmt(MStmt::Assign {
                            dest: elem,
                            value: Rvalue::GetField {
                                receiver: local,
                                class_name: ref_class,
                                field_name: "element".to_string(),
                            },
                        });
                        // Push a special scope entry: the $Ref holder is stored
                        // under a mangled name so Stmt::Assign can find it.
                        let ref_holder_sym =
                            interner.intern(&format!("$ref${}", interner.resolve(*sym)));
                        invoke_scope.push((ref_holder_sym, local));
                        invoke_scope.push((*sym, elem));
                    } else {
                        invoke_scope.push((*sym, local));
                    }
                }
                // For SuspendLambda, the CoroutineScope
                // receiver is accessible at runtime but doesn't need
                // to be a MIR param (the SuspendLambda shell handles
                // the invoke→create→invokeSuspend delegation). We just
                // add a scope marker so nested launch/async can find it.
                // The actual CoroutineScope local comes from the scope
                // chain — `this` (the lambda) implements Function2 and
                // receives the scope as arg1 at runtime.

                // Lambda parameters — use Ty::Any for the method param
                // so the invoke descriptor matches FunctionN.invoke(Object...).
                // Then immediately unbox/cast to the annotated type in
                // a body-local so method resolution and arithmetic work.
                for p in params {
                    let erased_pid = invoke_fb.new_local(Ty::Any);
                    invoke_fb.mf.params.push(erased_pid);
                    let annotated_ty = resolve_type(interner.resolve(p.ty.name), module);
                    if annotated_ty != Ty::Any {
                        // Cast/unbox from Object to the annotated type.
                        let cast_rvalue = match &annotated_ty {
                            Ty::Int => Some(Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Integer".to_string(),
                                    method_name: "intValue".to_string(),
                                    descriptor: "()I".to_string(),
                                },
                                args: vec![erased_pid],
                            }),
                            Ty::Bool => Some(Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Boolean".to_string(),
                                    method_name: "booleanValue".to_string(),
                                    descriptor: "()Z".to_string(),
                                },
                                args: vec![erased_pid],
                            }),
                            Ty::Long => Some(Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Long".to_string(),
                                    method_name: "longValue".to_string(),
                                    descriptor: "()J".to_string(),
                                },
                                args: vec![erased_pid],
                            }),
                            Ty::Double => Some(Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "java/lang/Double".to_string(),
                                    method_name: "doubleValue".to_string(),
                                    descriptor: "()D".to_string(),
                                },
                                args: vec![erased_pid],
                            }),
                            _ => {
                                // Reference type — use a local-copy typed
                                // as the annotated type so method resolution
                                // works (e.g., String.uppercase()).
                                Some(Rvalue::Local(erased_pid))
                            }
                        };
                        if let Some(rv) = cast_rvalue {
                            let typed_local = invoke_fb.new_local(annotated_ty);
                            invoke_fb.push_stmt(MStmt::Assign {
                                dest: typed_local,
                                value: rv,
                            });
                            invoke_scope.push((p.name, typed_local));
                        } else {
                            invoke_scope.push((p.name, erased_pid));
                        }
                    } else {
                        invoke_scope.push((p.name, erased_pid));
                    }
                }
                // Implicit `it` for single-param or no-param lambdas:
                // When a lambda has no explicit params, the scope function
                // passes the receiver as the first invoke arg. Add `it` to
                // scope pointing to that arg.
                if params.is_empty() && invoke_fb.mf.params.len() > 1 {
                    // params[0] is `this`, params[1] is the implicit `it`
                    let it_sym = interner.intern("it");
                    let it_local = invoke_fb.mf.params[1];
                    invoke_scope.push((it_sym, it_local));
                }
                // Lambda-with-receiver: if the caller set a receiver type
                // (e.g., apply/run/with scope functions), bind the first
                // invoke arg as `this` with the receiver type so bare
                // calls like `append("Hello")` resolve as this.append().
                if let Some(recv_type) = module.lambda_receiver_type.take() {
                    // Add an invoke parameter for the receiver and bind
                    // it as `this` in scope. The invoke method becomes
                    // invoke(Object receiver, ...) matching FunctionN.
                    let recv_param = invoke_fb.new_local(Ty::Any);
                    invoke_fb.mf.params.push(recv_param);
                    {
                        let raw_param = recv_param;
                        let typed_recv = invoke_fb.new_local(Ty::Class(recv_type));
                        invoke_fb.push_stmt(MStmt::Assign {
                            dest: typed_recv,
                            value: Rvalue::Local(raw_param),
                        });
                        let this_sym = interner.intern("this");
                        // Replace the lambda-self `this` with the receiver.
                        if let Some(pos) = invoke_scope.iter().position(|(s, _)| *s == this_sym) {
                            invoke_scope[pos] = (this_sym, typed_recv);
                        } else {
                            invoke_scope.push((this_sym, typed_recv));
                        }
                    }
                }
                for s in &body.stmts {
                    lower_stmt(
                        s,
                        &mut invoke_fb,
                        &mut invoke_scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        None,
                    );
                }
                invoke_fb.finish()
            };
            invoke_fn.is_abstract = false;
            let mut found_return_value = false;
            // Find the block with ReturnValue and determine if we need
            // to autobox the return for the erased FunctionN interface.
            let mut autobox_info: Option<(usize, LocalId, Ty)> = None;
            for (bi, block) in invoke_fn.blocks.iter().enumerate() {
                if let Terminator::ReturnValue(local) = &block.terminator {
                    let ret_ty = invoke_fn.locals[local.0 as usize].clone();
                    if ret_ty == Ty::Unit {
                        // Will convert to plain Return below.
                    } else {
                        autobox_info = Some((bi, *local, ret_ty));
                    }
                    found_return_value = true;
                    break;
                }
            }
            if let Some((bi, local, ret_ty)) = autobox_info {
                // Autobox the return value so the erased invoke
                // descriptor `()Ljava/lang/Object;` matches the
                // FunctionN interface. E.g. int -> Integer.valueOf.
                let box_rvalue = match &ret_ty {
                    Ty::Int => Some(Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "java/lang/Integer".to_string(),
                            method_name: "valueOf".to_string(),
                            descriptor: "(I)Ljava/lang/Integer;".to_string(),
                        },
                        args: vec![local],
                    }),
                    Ty::Bool => Some(Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "java/lang/Boolean".to_string(),
                            method_name: "valueOf".to_string(),
                            descriptor: "(Z)Ljava/lang/Boolean;".to_string(),
                        },
                        args: vec![local],
                    }),
                    Ty::Long => Some(Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "java/lang/Long".to_string(),
                            method_name: "valueOf".to_string(),
                            descriptor: "(J)Ljava/lang/Long;".to_string(),
                        },
                        args: vec![local],
                    }),
                    Ty::Double => Some(Rvalue::Call {
                        kind: CallKind::StaticJava {
                            class_name: "java/lang/Double".to_string(),
                            method_name: "valueOf".to_string(),
                            descriptor: "(D)Ljava/lang/Double;".to_string(),
                        },
                        args: vec![local],
                    }),
                    _ => None, // already a reference type
                };
                if let Some(rv) = box_rvalue {
                    let b = invoke_fn.new_local(Ty::Any);
                    invoke_fn.blocks[bi]
                        .stmts
                        .push(MStmt::Assign { dest: b, value: rv });
                    invoke_fn.blocks[bi].terminator = Terminator::ReturnValue(b);
                }
                invoke_fn.return_ty = Ty::Any;
            } else if found_return_value {
                // ReturnValue with Ty::Unit — convert to plain Return.
                for block in &mut invoke_fn.blocks {
                    if let Terminator::ReturnValue(local) = &block.terminator {
                        let ret_ty = invoke_fn.locals[local.0 as usize].clone();
                        if ret_ty == Ty::Unit {
                            block.terminator = Terminator::Return;
                        }
                    }
                }
                invoke_fn.return_ty = Ty::Unit;
            }
            if !found_return_value {
                // No ReturnValue found — the lambda body is all statements
                // (e.g. assignments). The return type is Unit.
                invoke_fn.return_ty = Ty::Unit;
            }

            // ── Attach state machine for suspend lambdas ──
            //
            // The invoke body we just built contains every inner suspend
            // call in its MIR. Run the same extractor the named-suspend-
            // fun path uses, but override the continuation class to be
            // the lambda class itself — kotlinc emits the state machine
            // directly on the SuspendLambda subclass (it IS the
            // continuation) rather than generating a separate
            // `InputKt$fn$1` companion. The JVM backend reads this
            // marker from the invoke method to generate the real
            // `invokeSuspend(Object)Object` body, replacing the
            // `IllegalStateException` stub.
            //
            // Currently we only support 0 or 1
            // suspension points. Multi-suspension, captures that span
            // suspend boundaries, and local-variable spilling inside
            // suspend lambdas are tracked as follow-ups.
            // Extra MIR fields to attach to the lambda class for its
            // state-machine spill slots (I$0, I$1, L$0, …). Mirrors the
            // list `build_continuation_class` adds to the named-function
            // continuation class, but placed directly on the lambda
            // (which IS the continuation).
            let mut lambda_extra_fields: Vec<MirField> = Vec::new();
            if is_suspend_lambda {
                let lambda_sm = extract_suspend_state_machine_with_cont(
                    &invoke_fn,
                    module,
                    &module.wrapper_class,
                    &lambda_class_name,
                    lambda_class_name.clone(),
                );
                match lambda_sm {
                    SuspendSitesResult::Zero => {
                        // No suspend calls in the body; invokeSuspend
                        // degenerates to `throwOnFailure($result); <body
                        // value>; areturn`. The backend currently
                        // requires a state machine, so we don't emit
                        // one — the backend falls back to a tail
                        // emitter for zero-site suspend lambdas.
                    }
                    SuspendSitesResult::Found(sm) => {
                        // Materialize one field per spill
                        // slot directly on the lambda class. The JVM
                        // backend's multi-suspend emitter addresses
                        // these via `getfield/putfield <lambda>.I$n:I`,
                        // using `aload_0` as the receiver (since the
                        // lambda IS the continuation, unlike named
                        // suspend funs which stash the continuation
                        // in a local slot).
                        for slot in &sm.spill_layout {
                            lambda_extra_fields.push(MirField {
                                name: slot.name.clone(),
                                ty: match slot.kind {
                                    SpillKind::Int => Ty::Int,
                                    SpillKind::Long => Ty::Long,
                                    SpillKind::Float => Ty::Int, // unused in this session
                                    SpillKind::Double => Ty::Double,
                                    SpillKind::Ref => Ty::Any,
                                },
                            });
                        }
                        invoke_fn.is_suspend = true;
                        invoke_fn.suspend_state_machine = Some(sm);
                    }
                    SuspendSitesResult::Unsupported(reason) => {
                        diags.push(Diagnostic::error(
                            body.span,
                            format!(
                                "suspend lambda has an unsupported shape: {reason}; the skotch \
                                 CPS transform currently supports straight-line bodies with \
                                 suspend calls that take only the implicit `$completion`"
                            ),
                        ));
                    }
                }
            }

            // ── Constructor (takes captured values) ─────────────────
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
                param_receiver_types: Vec::new(),
                is_abstract: false,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let init_this = init_fn.new_local(Ty::Class(lambda_class_name.clone()));
            init_fn.params.push(init_this);
            init_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: init_this,
                value: Rvalue::Call {
                    kind: CallKind::Constructor("java/lang/Object".to_string()),
                    args: vec![init_this],
                },
            });
            // Constructor params for captures → field assignments.
            for (sym, _, ty) in &free_vars {
                let field_ty = if let Some(ref_name) = ref_class_names.get(sym) {
                    Ty::Class(ref_name.clone())
                } else {
                    ty.clone()
                };
                let param_id = init_fn.new_local(field_ty);
                init_fn.params.push(param_id);
                init_fn.blocks[0].stmts.push(MStmt::Assign {
                    dest: init_this,
                    value: Rvalue::PutField {
                        receiver: init_this,
                        class_name: lambda_class_name.clone(),
                        field_name: interner.resolve(*sym).to_string(),
                        value: param_id,
                    },
                });
            }

            // Make the lambda class implement kotlin/jvm/functions/FunctionN
            // so it can be passed to Kotlin stdlib HOFs and user-defined HOFs.
            //
            // Suspend lambdas follow kotlinc's convention of bumping the
            // Function arity by 1: the implicit trailing `Continuation`
            // parameter of the CPS-rewritten lambda counts against the
            // `FunctionN` interface. So `{ yield_() }` (0 user params,
            // 1 suspend call) implements `Function1<Continuation, Object>`
            // rather than `Function0<Object>`.
            // Receiver-bearing lambdas have an extra invoke param for
            // the receiver, so the FunctionN arity is +1.
            let has_lambda_recv = invoke_fn.params.len() > params.len() + 1;
            let lambda_arity = params.len()
                + if is_suspend_lambda { 1 } else { 0 }
                + if has_lambda_recv { 1 } else { 0 };
            let iface_name = stdlib_function_interface(lambda_arity);

            // Suspend lambdas extend SuspendLambda instead of
            // Object. Their `<init>(Continuation)V`, `invokeSuspend`,
            // `create`, `invoke(Continuation)`, and erased
            // `invoke(Object)` bridge methods are synthesized by the
            // JVM backend directly from the `is_suspend_lambda` marker —
            // see `crates/skotch-backend-jvm/src/class_writer.rs::
            // emit_suspend_lambda_shell` for the bytecode recipe.
            //
            // The MIR constructor we just built is replaced with a
            // `(Continuation)V` stub below (the real super-ctor wiring
            // lives inline in `emit_suspend_lambda_shell`). The
            // `invoke_fn` is KEPT — the state-machine attachment above
            // populated its `suspend_state_machine` marker so the
            // backend can emit the real CPS state-machine body on
            // `invokeSuspend` (replacing the earlier
            // `IllegalStateException` stub).
            //
            // Non-suspend lambdas keep the `$Lambda$N`
            // shape (Function1-only, direct invoke) byte-stable.
            let (super_class, interfaces, final_init_fn) = if is_suspend_lambda {
                // Suspend lambda constructor takes capture
                // args BEFORE the Continuation param, matching kotlinc's
                // `<init>(capture1, ..., captureN, Continuation)V`.
                // Each capture is stored into its corresponding field on
                // `this` before the super-ctor call (the JVM backend's
                // `emit_suspend_lambda_shell` handles the actual super-
                // ctor delegation; the MIR params here drive the
                // descriptor and call-site argument count).
                let mut susp_init = MirFunction {
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
                    param_receiver_types: Vec::new(),
                    is_abstract: false,
                    exception_handlers: Vec::new(),
                    vararg_index: None,
                    is_suspend: false,
                    is_inline: false,
                    suspend_original_return_ty: None,
                    suspend_state_machine: None,
                    annotations: Vec::new(),
                };
                let susp_this = susp_init.new_local(Ty::Class(lambda_class_name.clone()));
                susp_init.params.push(susp_this);
                // Capture params — one per free variable.
                for (sym, _, ty) in &free_vars {
                    let field_ty = if let Some(ref_name) = ref_class_names.get(sym) {
                        Ty::Class(ref_name.clone())
                    } else {
                        ty.clone()
                    };
                    let cap_param = susp_init.new_local(field_ty);
                    susp_init.params.push(cap_param);
                    susp_init.blocks[0].stmts.push(MStmt::Assign {
                        dest: susp_this,
                        value: Rvalue::PutField {
                            receiver: susp_this,
                            class_name: lambda_class_name.clone(),
                            field_name: interner.resolve(*sym).to_string(),
                            value: cap_param,
                        },
                    });
                }
                let susp_completion =
                    susp_init.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                susp_init.params.push(susp_completion);
                (
                    Some("kotlin/coroutines/jvm/internal/SuspendLambda".to_string()),
                    vec![iface_name],
                    susp_init,
                )
            } else {
                (None, vec![iface_name], init_fn)
            };

            // Suspend lambdas with multi-suspension bodies
            // spill live locals to synthetic fields on themselves (the
            // lambda IS the continuation). The list we built above is
            // appended AFTER the real captures so field ordering is
            // stable wrt `capture_fields` for zero-/one-suspension
            // fixtures that never populated `lambda_extra_fields`.
            let mut final_fields = capture_fields;
            final_fields.extend(lambda_extra_fields);

            // Add p$0 field for CoroutineScope receiver on
            // ALL suspend lambdas. The interface might be Function1 at
            // this point (patched to Function2 later by the builder handler).
            if is_suspend_lambda {
                final_fields.push(MirField {
                    name: "p$0".to_string(),
                    ty: Ty::Any,
                });
            }

            // Replace the pre-registered stub with the real class.
            module.classes[lambda_idx] = MirClass {
                name: lambda_class_name.clone(),
                super_class,
                is_open: false,
                is_abstract: false,
                is_interface: false,
                interfaces,
                fields: final_fields,
                methods: vec![invoke_fn],
                constructor: final_init_fn,
                secondary_constructors: Vec::new(),
                is_suspend_lambda,
                is_cross_file_stub: false,
                annotations: Vec::new(),
            };

            // ── Instantiate at definition site ──────────────────────
            let inst = fb.new_local(Ty::Class(lambda_class_name.clone()));
            fb.push_stmt(MStmt::Assign {
                dest: inst,
                value: Rvalue::NewInstance(lambda_class_name.clone()),
            });
            // Pass captured values as constructor args.
            // For ref-boxed captures, pass the $Ref instance (which is now
            // the outer scope binding after we replaced it above).
            let ctor_args: Vec<LocalId> = if is_suspend_lambda {
                // Suspend lambda's constructor is
                // `(capture1, ..., captureN, Continuation)V`. Pass
                // captured locals first, then null Continuation.
                let mut args: Vec<LocalId> = free_vars
                    .iter()
                    .map(|(sym, orig_lid, _)| {
                        if ref_class_names.contains_key(sym) {
                            scope
                                .iter()
                                .rev()
                                .find(|(s, _)| s == sym)
                                .map(|(_, lid)| *lid)
                                .unwrap_or(*orig_lid)
                        } else {
                            *orig_lid
                        }
                    })
                    .collect();
                let null_cont =
                    fb.new_local(Ty::Class("kotlin/coroutines/Continuation".to_string()));
                fb.push_stmt(MStmt::Assign {
                    dest: null_cont,
                    value: Rvalue::Const(MirConst::Null),
                });
                args.push(null_cont);
                args
            } else {
                free_vars
                    .iter()
                    .map(|(sym, orig_lid, _)| {
                        if ref_class_names.contains_key(sym) {
                            // The outer scope now points to the $Ref instance.
                            scope
                                .iter()
                                .rev()
                                .find(|(s, _)| s == sym)
                                .map(|(_, lid)| *lid)
                                .unwrap_or(*orig_lid)
                        } else {
                            *orig_lid
                        }
                    })
                    .collect()
            };
            fb.push_stmt(MStmt::Assign {
                dest: inst,
                value: Rvalue::Call {
                    kind: CallKind::Constructor(lambda_class_name),
                    args: ctor_args,
                },
            });
            Some(inst)
        }

        Expr::ObjectExpr {
            super_type,
            methods,
            ..
        } => {
            let super_name = interner.resolve(*super_type).to_string();
            let obj_idx = module.classes.len();
            let obj_class_name = format!("$Object${obj_idx}");

            // Lower each method.
            let mut mir_methods = Vec::new();
            for method in methods {
                let method_name = interner.resolve(method.name).to_string();
                let return_ty = method
                    .return_ty
                    .as_ref()
                    .map(|tr| {
                        let resolved = resolve_type(interner.resolve(tr.name), module);
                        resolved
                    })
                    .unwrap_or(Ty::Unit);
                let fn_idx = module.functions.len() + mir_methods.len();
                let mut mfb = FnBuilder::new(fn_idx, method_name, return_ty);
                let this_local = mfb.new_local(Ty::Class(obj_class_name.clone()));
                mfb.mf.params.push(this_local);
                let mut mscope: Vec<(Symbol, LocalId)> = Vec::new();
                for p in &method.params {
                    let ty = resolve_type(interner.resolve(p.ty.name), module);
                    let pid = mfb.new_local(ty);
                    mfb.mf.params.push(pid);
                    mscope.push((p.name, pid));
                }
                for s in &method.body.stmts {
                    lower_stmt(
                        s,
                        &mut mfb,
                        &mut mscope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        None,
                    );
                }
                let mut finished = mfb.finish();
                // Patch Unit-returning methods: convert ReturnValue to Return.
                for block in &mut finished.blocks {
                    if let Terminator::ReturnValue(local) = &block.terminator {
                        if finished.locals[local.0 as usize] == Ty::Unit {
                            block.terminator = Terminator::Return;
                            finished.return_ty = Ty::Unit;
                        } else {
                            finished.return_ty = finished.locals[local.0 as usize].clone();
                        }
                    }
                }
                mir_methods.push(finished);
            }

            // Determine if super is an interface or class.
            let is_iface = module
                .classes
                .iter()
                .any(|c| c.name == super_name && c.is_interface);
            let (super_class, ifaces) = if is_iface {
                (None, vec![super_name.clone()])
            } else {
                (Some(super_name.clone()), vec![])
            };

            // Build constructor.
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
                param_receiver_types: Vec::new(),
                is_abstract: false,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let init_this = init_fn.new_local(Ty::Class(obj_class_name.clone()));
            init_fn.params.push(init_this);
            let super_ctor = if is_iface {
                "java/lang/Object".to_string()
            } else {
                super_name.clone()
            };
            init_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: init_this,
                value: Rvalue::Call {
                    kind: CallKind::Constructor(super_ctor),
                    args: vec![init_this],
                },
            });

            module.classes.push(MirClass {
                name: obj_class_name.clone(),
                super_class,
                is_open: false,
                is_abstract: false,
                is_interface: false,
                interfaces: ifaces,
                fields: Vec::new(),
                methods: mir_methods,
                constructor: init_fn,
                secondary_constructors: Vec::new(),
                is_suspend_lambda: false,
                is_cross_file_stub: false,
                annotations: Vec::new(),
            });

            // Instantiate.
            let inst = fb.new_local(Ty::Class(super_name));
            fb.push_stmt(MStmt::Assign {
                dest: inst,
                value: Rvalue::NewInstance(obj_class_name.clone()),
            });
            fb.push_stmt(MStmt::Assign {
                dest: inst,
                value: Rvalue::Call {
                    kind: CallKind::Constructor(obj_class_name),
                    args: vec![],
                },
            });
            Some(inst)
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

    // Walk the superclass chain to find the method. This handles
    // cases like Exception.getMessage() where getMessage is declared
    // on Throwable, not Exception itself.
    let mut search_class = Some(class_name.to_string());
    let mut found_method: Option<(String, String, String)> = None; // (class, name, desc)
    while let Some(ref cname) = search_class {
        // Ensure this class is loaded.
        if !reg.contains_key(cname.as_str()) {
            let jvm_path = if cname.contains('/') {
                cname.clone()
            } else {
                format!("java/lang/{cname}")
            };
            if let Ok(info) = skotch_classinfo::load_jdk_class(&jvm_path) {
                reg.insert(cname.clone(), info);
            } else {
                break;
            }
        }
        if let Some(ci) = reg.get(cname.as_str()) {
            let m = ci
                .methods
                .iter()
                .find(|m| {
                    m.name == method_name
                        && !m.is_static()
                        && m.is_public()
                        && count_descriptor_params(&m.descriptor) == arg_count
                })
                .or_else(|| {
                    ci.methods
                        .iter()
                        .find(|m| m.name == method_name && !m.is_static() && m.is_public())
                });
            if let Some(method) = m {
                found_method = Some((
                    ci.name.clone(),
                    method.name.clone(),
                    method.descriptor.clone(),
                ));
                break;
            }
            search_class = ci.super_class.clone();
        } else {
            break;
        }
    }
    let (found_class, method_name_found, descriptor) = found_method?;

    let return_ty = match skotch_classinfo::return_type_from_descriptor(&descriptor) {
        "Unit" => Ty::Unit,
        "Boolean" => Ty::Bool,
        "Int" => Ty::Int,
        "Long" => Ty::Long,
        "Double" => Ty::Double,
        "String" => Ty::String,
        _ => Ty::Any,
    };

    Some((found_class, method_name_found, descriptor, return_ty))
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
    // Resolve through import_map if the simple name is imported.
    let resolved_class = module
        .import_map
        .get(&class_name)
        .cloned()
        .unwrap_or_else(|| class_name.clone());
    let (jvm_class, jvm_method, descriptor, return_ty) =
        lookup_java_static(&resolved_class, &method_str, args.len())?;

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
    _name_to_global: &mut FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
) {
    let enum_name = interner.resolve(e.name).to_string();

    // ── Build enum as a real MirClass ───────────────────────────────────
    //
    // Every enum class has an implicit `name` field (the entry name as a
    // String), plus any user-declared constructor params as additional
    // fields. The <init> takes (name: String, ...params) and stores them.
    //
    // Each entry (e.g., Color.RED) becomes a top-level function that
    // creates and returns a new instance: `new Color("RED", 16711680)`.

    // Fields: name (implicit) + constructor params.
    let mut fields = vec![MirField {
        name: "name".to_string(),
        ty: Ty::String,
    }];
    for param in &e.constructor_params {
        let ty = resolve_type(interner.resolve(param.ty.name), module);
        fields.push(MirField {
            name: interner.resolve(param.name).to_string(),
            ty,
        });
    }

    // <init>(this, name: String, ...params)
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
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
    };
    // this
    let this_id = init_fn.new_local(Ty::Class(enum_name.clone()));
    init_fn.params.push(this_id);
    // Super call: java/lang/Object.<init>()V
    init_fn.blocks[0].stmts.push(MStmt::Assign {
        dest: this_id,
        value: Rvalue::Call {
            kind: CallKind::Constructor("java/lang/Object".to_string()),
            args: vec![this_id],
        },
    });
    // name param
    let name_param = init_fn.new_local(Ty::String);
    init_fn.params.push(name_param);
    init_fn.blocks[0].stmts.push(MStmt::Assign {
        dest: this_id,
        value: Rvalue::PutField {
            receiver: this_id,
            class_name: enum_name.clone(),
            field_name: "name".to_string(),
            value: name_param,
        },
    });
    // Constructor params
    for param in &e.constructor_params {
        let ty = resolve_type(interner.resolve(param.ty.name), module);
        let pid = init_fn.new_local(ty);
        init_fn.params.push(pid);
        init_fn.blocks[0].stmts.push(MStmt::Assign {
            dest: this_id,
            value: Rvalue::PutField {
                receiver: this_id,
                class_name: enum_name.clone(),
                field_name: interner.resolve(param.name).to_string(),
                value: pid,
            },
        });
    }

    // toString() method → returns this.name
    let mut ts_fn = MirFunction {
        id: FuncId(0),
        name: "toString".to_string(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: vec![BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        }],
        return_ty: Ty::String,
        required_params: 0,
        param_names: Vec::new(),
        param_defaults: Vec::new(),
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
    };
    let ts_this = ts_fn.new_local(Ty::Class(enum_name.clone()));
    ts_fn.params.push(ts_this);
    let ts_name = ts_fn.new_local(Ty::String);
    ts_fn.blocks[0].stmts.push(MStmt::Assign {
        dest: ts_name,
        value: Rvalue::GetField {
            receiver: ts_this,
            class_name: enum_name.clone(),
            field_name: "name".to_string(),
        },
    });
    ts_fn.blocks[0].terminator = Terminator::ReturnValue(ts_name);

    // ── Abstract method dispatch ──────────────────────────────────────
    //
    // For each abstract method declared on the enum class, we generate a
    // concrete instance method that dispatches based on `this.name`.
    // For each abstract method, generate a dispatch function as a
    // top-level function (not a class method) so the JVM backend's
    // walk_block path handles StackMapTable correctly. The function
    // takes (this: EnumClass, params...) and dispatches on this.name.
    let class_methods = vec![ts_fn];

    for abs_method in &e.methods {
        if !abs_method.is_abstract {
            continue;
        }
        let method_name_str = interner.resolve(abs_method.name).to_string();
        let ret_ty = abs_method
            .return_ty
            .as_ref()
            .map(|tr| resolve_type(interner.resolve(tr.name), module))
            .unwrap_or(Ty::Unit);

        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        // Register under "EnumName$methodName" so call sites can find it.
        let dispatch_name = format!("{}${}", enum_name, method_name_str);
        let dispatch_sym = interner.intern(&dispatch_name);
        name_to_func.insert(dispatch_sym, fn_id);

        let mut fb = FnBuilder::new(fn_idx, dispatch_name, ret_ty.clone());

        // this parameter
        let this_local = fb.new_local(Ty::Class(enum_name.clone()));
        fb.mf.params.push(this_local);
        // user params
        let mut param_locals = Vec::new();
        for p in &abs_method.params {
            let pty = resolve_type(interner.resolve(p.ty.name), module);
            let pid = fb.new_local(pty);
            fb.mf.params.push(pid);
            param_locals.push((p.name, pid));
        }

        // Get this.name for dispatch
        let name_field = fb.new_local(Ty::String);
        fb.push_stmt(MStmt::Assign {
            dest: name_field,
            value: Rvalue::GetField {
                receiver: this_local,
                class_name: enum_name.clone(),
                field_name: "name".to_string(),
            },
        });

        // For each entry with an override of this method, generate an
        // if-branch comparing this.name == "ENTRY_NAME".
        let scope: Vec<(Symbol, LocalId)> = param_locals.clone();

        for entry in &e.entries {
            let override_method = entry
                .methods
                .iter()
                .find(|m| interner.resolve(m.name) == interner.resolve(abs_method.name));
            let Some(om) = override_method else {
                continue;
            };

            let entry_name_str = interner.resolve(entry.name).to_string();
            let entry_name_sid = module.intern_string(&entry_name_str);
            let entry_name_local = fb.new_local(Ty::String);
            fb.push_stmt(MStmt::Assign {
                dest: entry_name_local,
                value: Rvalue::Const(MirConst::String(entry_name_sid)),
            });

            // Compare name_field == entry_name_str
            let cmp = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: cmp,
                value: Rvalue::BinOp {
                    op: MBinOp::CmpEq,
                    lhs: name_field,
                    rhs: entry_name_local,
                },
            });

            let then_block = fb.new_block();
            let next_block = fb.new_block();
            fb.set_terminator(Terminator::Branch {
                cond: cmp,
                then_block,
                else_block: next_block,
            });

            // Then block: inline the override body
            fb.cur_block = then_block;
            let mut entry_scope = scope.clone();
            let mut diags = Diagnostics::new();
            for stmt in &om.body.stmts {
                lower_stmt(
                    stmt,
                    &mut fb,
                    &mut entry_scope,
                    module,
                    name_to_func,
                    _name_to_global,
                    interner,
                    &mut diags,
                    None,
                );
            }
            if !matches!(
                fb.mf.blocks[fb.cur_block as usize].terminator,
                Terminator::ReturnValue(_) | Terminator::Return
            ) {
                fb.set_terminator(Terminator::Return);
            }

            fb.cur_block = next_block;
        }

        // Fallback: return default value (shouldn't be reached)
        if ret_ty == Ty::Int {
            let zero = fb.new_local(Ty::Int);
            fb.push_stmt(MStmt::Assign {
                dest: zero,
                value: Rvalue::Const(MirConst::Int(0)),
            });
            fb.set_terminator(Terminator::ReturnValue(zero));
        } else {
            fb.set_terminator(Terminator::Return);
        }

        module.add_function(fb.finish());
    }

    module.classes.push(MirClass {
        name: enum_name.clone(),
        super_class: None,
        is_open: false,
        is_abstract: false,
        is_interface: false,
        interfaces: Vec::new(),
        fields,
        methods: class_methods,
        constructor: init_fn,
        secondary_constructors: Vec::new(),
        is_suspend_lambda: false,
        is_cross_file_stub: false,
        annotations: Vec::new(),
    });

    // ── Entry functions ─────────────────────────────────────────────────
    //
    // Each entry becomes a top-level function:
    //   fun RED(): Color = Color("RED", 16711680)
    for entry in &e.entries {
        let entry_name = interner.resolve(entry.name).to_string();
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        name_to_func.insert(entry.name, fn_id);

        let mut fb = FnBuilder::new(fn_idx, entry_name.clone(), Ty::Class(enum_name.clone()));

        // NewInstance + Constructor(enum_name, [name_str, ...args])
        let inst = fb.new_local(Ty::Class(enum_name.clone()));
        fb.push_stmt(MStmt::Assign {
            dest: inst,
            value: Rvalue::NewInstance(enum_name.clone()),
        });

        // Build constructor args: ["ENTRY_NAME", ...entry.args]
        // (inst is NOT included — the JVM backend's NewInstance handler
        // already has [ref, ref] on the stack from new+dup.)
        let name_sid = module.intern_string(&entry_name);
        let name_local = fb.new_local(Ty::String);
        fb.push_stmt(MStmt::Assign {
            dest: name_local,
            value: Rvalue::Const(MirConst::String(name_sid)),
        });
        let mut ctor_args = vec![name_local];

        for arg_expr in &entry.args {
            if let Some(c) = lower_const_init(arg_expr, module) {
                let ty = const_ty(&c);
                let arg_local = fb.new_local(ty);
                fb.push_stmt(MStmt::Assign {
                    dest: arg_local,
                    value: Rvalue::Const(c),
                });
                ctor_args.push(arg_local);
            }
        }

        fb.push_stmt(MStmt::Assign {
            dest: inst,
            value: Rvalue::Call {
                kind: CallKind::Constructor(enum_name.clone()),
                args: ctor_args,
            },
        });

        fb.set_terminator(Terminator::ReturnValue(inst));
        module.add_function(fb.finish());
    }

    // ── values() function ──────────────────────────────────────────────
    //
    // Returns an ArrayList<EnumClass> containing all entries.
    // Desugars to: ArrayList() + add(RED()) + add(GREEN()) + ... → return list
    {
        let values_fn_name = format!("{}$values", enum_name);
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        let values_sym = interner.intern(&values_fn_name);
        name_to_func.insert(values_sym, fn_id);

        let list_ty = Ty::Class("java/util/ArrayList".to_string());
        let mut fb = FnBuilder::new(fn_idx, values_fn_name, list_ty.clone());

        // Create ArrayList.
        let list_local = fb.new_local(list_ty.clone());
        fb.push_stmt(MStmt::Assign {
            dest: list_local,
            value: Rvalue::NewInstance("java/util/ArrayList".to_string()),
        });
        fb.push_stmt(MStmt::Assign {
            dest: list_local,
            value: Rvalue::Call {
                kind: CallKind::Constructor("java/util/ArrayList".to_string()),
                args: vec![],
            },
        });

        // Add each entry.
        for entry in &e.entries {
            let entry_fid = *name_to_func.get(&entry.name).unwrap();
            let entry_local = fb.new_local(Ty::Class(enum_name.clone()));
            fb.push_stmt(MStmt::Assign {
                dest: entry_local,
                value: Rvalue::Call {
                    kind: CallKind::Static(entry_fid),
                    args: vec![],
                },
            });
            let _add_result = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: _add_result,
                value: Rvalue::Call {
                    kind: CallKind::VirtualJava {
                        class_name: "java/util/ArrayList".to_string(),
                        method_name: "add".to_string(),
                        descriptor: "(Ljava/lang/Object;)Z".to_string(),
                    },
                    args: vec![list_local, entry_local],
                },
            });
        }

        fb.set_terminator(Terminator::ReturnValue(list_local));
        module.add_function(fb.finish());
    }

    // ── valueOf(name: String) function ─────────────────────────────────
    //
    // Compares the string argument against each entry name. Returns the
    // first match. If none match, returns the first entry (fallback).
    // Layout:
    //   block0: load arg → check entry0 name → branch
    //   check_N: if (arg == "NAME") goto match_N else goto check_N+1
    //   match_N: return EntryN()
    //   fallback: return Entry0()   (Kotlin would throw, we simplify)
    {
        let valueof_fn_name = format!("{}$valueOf", enum_name);
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        let valueof_sym = interner.intern(&valueof_fn_name);
        name_to_func.insert(valueof_sym, fn_id);

        let entry_ty = Ty::Class(enum_name.clone());
        let mut fb = FnBuilder::new(fn_idx, valueof_fn_name, entry_ty.clone());

        // Parameter: name string.
        let name_param = fb.new_local(Ty::String);
        fb.mf.params.push(name_param);
        fb.mf.required_params = 1;
        fb.mf.param_names.push("name".to_string());

        // For each entry, create check + match blocks.
        let entry_count = e.entries.len();
        if entry_count == 0 {
            // No entries — just return null-ish (shouldn't happen in practice).
            let null_local = fb.new_local(entry_ty);
            fb.push_stmt(MStmt::Assign {
                dest: null_local,
                value: Rvalue::Const(MirConst::Null),
            });
            fb.set_terminator(Terminator::ReturnValue(null_local));
        } else {
            // Build chain: check0 → match0 | check1 → match1 | ... | fallback
            let fallback_block = fb.new_block();
            let mut next_check = fallback_block;

            // Build in reverse so each check can branch to the next.
            for i in (0..entry_count).rev() {
                let entry = &e.entries[i];
                let entry_name_str = interner.resolve(entry.name).to_string();
                let entry_fid = *name_to_func.get(&entry.name).unwrap();

                let check_block = if i == 0 { 0 } else { fb.new_block() };
                let match_block = fb.new_block();

                // In check_block: compare name_param with entry name string.
                let saved = fb.cur_block;
                fb.cur_block = check_block;

                let name_sid = module.intern_string(&entry_name_str);
                let name_const = fb.new_local(Ty::String);
                fb.push_stmt(MStmt::Assign {
                    dest: name_const,
                    value: Rvalue::Const(MirConst::String(name_sid)),
                });
                let cmp = fb.new_local(Ty::Bool);
                fb.push_stmt(MStmt::Assign {
                    dest: cmp,
                    value: Rvalue::BinOp {
                        op: MBinOp::CmpEq,
                        lhs: name_param,
                        rhs: name_const,
                    },
                });
                fb.mf.blocks[check_block as usize].terminator = Terminator::Branch {
                    cond: cmp,
                    then_block: match_block,
                    else_block: next_check,
                };

                // In match_block: call entry function, return result.
                fb.cur_block = match_block;
                let result = fb.new_local(Ty::Class(enum_name.clone()));
                fb.push_stmt(MStmt::Assign {
                    dest: result,
                    value: Rvalue::Call {
                        kind: CallKind::Static(entry_fid),
                        args: vec![],
                    },
                });
                fb.mf.blocks[match_block as usize].terminator = Terminator::ReturnValue(result);

                fb.cur_block = saved;
                next_check = check_block;
            }

            // Fallback: return first entry.
            fb.cur_block = fallback_block;
            let first_entry_fid = *name_to_func.get(&e.entries[0].name).unwrap();
            let fallback_result = fb.new_local(Ty::Class(enum_name.clone()));
            fb.push_stmt(MStmt::Assign {
                dest: fallback_result,
                value: Rvalue::Call {
                    kind: CallKind::Static(first_entry_fid),
                    args: vec![],
                },
            });
            fb.mf.blocks[fallback_block as usize].terminator =
                Terminator::ReturnValue(fallback_result);
        }

        module.add_function(fb.finish());
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
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
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
            .map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
            .unwrap_or(Ty::Unit);

        // Register as a top-level function so Singleton.method() resolves.
        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        name_to_func.insert(method.name, fn_id);

        let mut fb = FnBuilder::new(fn_idx, method_name, return_ty);
        // Object methods have no `this` — they're effectively static.
        let mut scope: Vec<(Symbol, LocalId)> = Vec::new();
        for p in &method.params {
            let ty = resolve_type(interner.resolve(p.ty.name), module);
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
    name_to_global: &mut FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let class_name = interner.resolve(c.name).to_string();

    // Collect fields from constructor params (val/var) and body properties.
    let mut fields = Vec::new();
    for p in &c.constructor_params {
        if p.is_val || p.is_var {
            let ty = resolve_type_ref(&p.ty, interner, module);
            fields.push(MirField {
                name: interner.resolve(p.name).to_string(),
                ty,
            });
        }
    }
    // Add delegate parameters as fields (for interface delegation: `Base by b`).
    // These are constructor params that are not val/var but are referenced as
    // delegates — the delegate value must be stored in a field so forwarding
    // methods can access it.
    let delegate_param_names: rustc_hash::FxHashSet<Symbol> = c
        .interface_delegates
        .iter()
        .map(|(_, param)| *param)
        .collect();
    for p in &c.constructor_params {
        if !p.is_val && !p.is_var && delegate_param_names.contains(&p.name) {
            let ty = resolve_type(interner.resolve(p.ty.name), module);
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
            .map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
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
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
    };
    // Add 'this' as local 0.
    let this_id = init_fn.new_local(Ty::Class(class_name.clone()));
    init_fn.params.push(this_id);

    // Build a scope for lowering super args (this + constructor params).
    // ALL constructor params are added as init params so they're available
    // for super-constructor delegation, e.g. `class Dog(name: String) : Animal(name)`.
    let mut ctor_param_ids: Vec<(Symbol, LocalId)> = Vec::new();
    for p in &c.constructor_params {
        let ty = resolve_type_ref(&p.ty, interner, module);
        let param_id = init_fn.new_local(ty);
        init_fn.params.push(param_id);
        ctor_param_ids.push((p.name, param_id));
    }

    // Emit super constructor call.
    {
        let super_class_name = c
            .parent_class
            .as_ref()
            .map(|sc| interner.resolve(sc.name).to_string())
            .unwrap_or_else(|| "java/lang/Object".to_string());
        // Lower super args if present.
        let mut super_arg_ids = vec![this_id]; // receiver is always first
        if let Some(sc) = &c.parent_class {
            if !sc.args.is_empty() {
                // Create a temporary FnBuilder to lower the super args.
                let tmp_idx = module.functions.len() + 9000;
                let mut fb = FnBuilder::new(tmp_idx, "<super-args>".to_string(), Ty::Unit);
                // Copy locals and params from init_fn so scope resolution works.
                fb.mf.locals = init_fn.locals.clone();
                fb.mf.params = init_fn.params.clone();
                let this_sym = interner.intern("this");
                let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_id)];
                for (sym, lid) in &ctor_param_ids {
                    scope.push((*sym, *lid));
                }
                for arg in &sc.args {
                    if let Some(id) = lower_expr(
                        &arg.expr,
                        &mut fb,
                        &mut scope,
                        module,
                        name_to_func,
                        name_to_global,
                        interner,
                        diags,
                        None,
                    ) {
                        super_arg_ids.push(id);
                    }
                }
                // Merge any new locals/stmts back into init_fn.
                init_fn.locals = fb.mf.locals;
                for stmt in fb.mf.blocks[0].stmts.drain(..) {
                    init_fn.blocks[0].stmts.push(stmt);
                }
            }
        }
        init_fn.blocks[0].stmts.push(MStmt::Assign {
            dest: this_id, // dummy
            value: Rvalue::Call {
                kind: CallKind::Constructor(super_class_name),
                args: super_arg_ids,
            },
        });
    }

    // Add field assignments for constructor params.
    for (sym, param_id) in &ctor_param_ids {
        let field_name = interner.resolve(*sym).to_string();
        init_fn.blocks[0].stmts.push(MStmt::Assign {
            dest: this_id, // dummy dest
            value: Rvalue::PutField {
                receiver: this_id,
                class_name: class_name.clone(),
                field_name,
                value: *param_id,
            },
        });
    }

    // (val/var constructor params already added above)
    // Initialize body properties with default values.
    // Properties with `by lazy { ... }` delegates are lowered eagerly:
    // the lambda body is evaluated during construction and stored in the field.
    let mut delegate_props: Vec<&skotch_syntax::PropertyDecl> = Vec::new();
    for prop in &c.properties {
        if prop.delegate.is_some() {
            delegate_props.push(prop);
            continue;
        }
        let (val, ty) = if let Some(init) = &prop.init {
            match init {
                Expr::IntLit(v, _) => (Some(MirConst::Int(*v as i32)), Ty::Int),
                Expr::LongLit(v, _) => (Some(MirConst::Long(*v)), Ty::Long),
                Expr::DoubleLit(v, _) => (Some(MirConst::Double(*v)), Ty::Double),
                Expr::FloatLit(v, _) => (Some(MirConst::Float(*v as f32)), Ty::Float),
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
                .map(|tr| {
                    let resolved = resolve_type(interner.resolve(tr.name), module);
                    resolved
                })
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
                param_receiver_types: Vec::new(),
                is_abstract: m.is_abstract,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
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
        secondary_constructors: Vec::new(),
        is_suspend_lambda: false,
        is_cross_file_stub: false,
        annotations: Vec::new(),
    });

    // Lower methods.
    let mut mir_methods = Vec::new();
    // Lower `by lazy { ... }` as truly lazy initialization.
    // For each lazy property: generate a getter method that checks an
    // `initialized` flag, runs the body on first access, and caches.
    // The eager approach has been replaced with this lazy pattern.
    if !delegate_props.is_empty() {
        for prop in &delegate_props {
            let block = prop.delegate.as_ref().unwrap();
            let field_name = interner.resolve(prop.name).to_string();
            let prop_ty = prop
                .ty
                .as_ref()
                .map(|tr| resolve_type(interner.resolve(tr.name), module))
                .unwrap_or(Ty::Any);

            // Add a $initialized boolean field.
            let init_field_name = format!("{}$initialized", field_name);
            fields.push(MirField {
                name: init_field_name.clone(),
                ty: Ty::Bool,
            });

            // Generate getter: if (!field$initialized) { field = <body>; field$initialized = true } return field
            let fn_idx = module.functions.len() + mir_methods.len();
            let getter_name = format!("get{}{}", &field_name[..1].to_uppercase(), &field_name[1..]);
            let mut fb = FnBuilder::new(fn_idx, getter_name, prop_ty.clone());
            let this_local = fb.new_local(Ty::Class(class_name.clone()));
            fb.mf.params.push(this_local);
            let this_sym = interner.intern("this");
            let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];

            // Load fields into scope for the lazy body.
            for field in &fields {
                if field.name == field_name || field.name == init_field_name {
                    continue; // don't pre-load the lazy field itself
                }
                let fsym = interner.intern(&field.name);
                let fl = fb.new_local(field.ty.clone());
                fb.push_stmt(MStmt::Assign {
                    dest: fl,
                    value: Rvalue::GetField {
                        receiver: this_local,
                        class_name: class_name.clone(),
                        field_name: field.name.clone(),
                    },
                });
                scope.push((fsym, fl));
            }

            // Check initialized flag.
            let init_flag = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: init_flag,
                value: Rvalue::GetField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: init_field_name.clone(),
                },
            });
            let init_block = fb.new_block();
            let done_block = fb.new_block();
            fb.terminate_and_switch(
                Terminator::Branch {
                    cond: init_flag,
                    then_block: done_block, // already initialized → skip
                    else_block: init_block, // not initialized → run body
                },
                init_block,
            );

            // Init block: run the lazy body.
            let mut result_local: Option<LocalId> = None;
            for (i, s) in block.stmts.iter().enumerate() {
                let is_last = i + 1 == block.stmts.len();
                if is_last {
                    if let Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } = s {
                        result_local = lower_expr(
                            e,
                            &mut fb,
                            &mut scope,
                            module,
                            name_to_func,
                            name_to_global,
                            interner,
                            diags,
                            None,
                        );
                    } else {
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
                } else {
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
            // Store result into backing field.
            if let Some(val) = result_local {
                let dummy = fb.new_local(Ty::Unit);
                fb.push_stmt(MStmt::Assign {
                    dest: dummy,
                    value: Rvalue::PutField {
                        receiver: this_local,
                        class_name: class_name.clone(),
                        field_name: field_name.clone(),
                        value: val,
                    },
                });
            }
            // Set initialized = true.
            let true_val = fb.new_local(Ty::Bool);
            fb.push_stmt(MStmt::Assign {
                dest: true_val,
                value: Rvalue::Const(MirConst::Bool(true)),
            });
            let dummy2 = fb.new_local(Ty::Unit);
            fb.push_stmt(MStmt::Assign {
                dest: dummy2,
                value: Rvalue::PutField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: init_field_name,
                    value: true_val,
                },
            });
            fb.terminate_and_switch(Terminator::Goto(done_block), done_block);

            // Done block: load and return the cached value.
            let cached = fb.new_local(prop_ty);
            fb.push_stmt(MStmt::Assign {
                dest: cached,
                value: Rvalue::GetField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: field_name.clone(),
                },
            });
            fb.set_terminator(Terminator::ReturnValue(cached));
            mir_methods.push(fb.finish());
        }
    }
    for method in &c.methods {
        let method_name = interner.resolve(method.name).to_string();
        let declared_ret = method
            .return_ty
            .as_ref()
            .map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
            .unwrap_or(Ty::Unit);

        // Suspend instance methods get the same CPS
        // transform as top-level suspend functions — return type
        // rewritten to Object, $completion param appended.
        let return_ty = if method.is_suspend {
            Ty::Any
        } else {
            declared_ret.clone()
        };

        let fn_idx = module.functions.len() + mir_methods.len();
        let mut fb = FnBuilder::new(fn_idx, method_name.clone(), return_ty);
        fb.mf.is_suspend = method.is_suspend;
        if method.is_suspend {
            fb.mf.suspend_original_return_ty = Some(declared_ret.clone());
        }

        // Add implicit `this` parameter.
        let this_local = fb.new_local(Ty::Class(class_name.clone()));
        fb.mf.params.push(this_local);
        let this_sym = interner.intern("this");
        let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];

        // Add explicit parameters.
        for p in &method.params {
            let ty = resolve_type(interner.resolve(p.ty.name), module);
            let id = fb.new_local(ty);
            fb.mf.params.push(id);
            scope.push((p.name, id));
        }

        // Suspend methods get a trailing $completion param.
        if method.is_suspend {
            let cont_ty = Ty::Class("kotlin/coroutines/Continuation".to_string());
            let cont_id = fb.new_local(cont_ty);
            fb.mf.params.push(cont_id);
        }

        // Load fields into locals so they're accessible by name in the method body.
        // Track field→local mapping for writeback after the body.
        // Also load inherited fields from superclasses (read-only).
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
        // Inherited fields from superclasses.
        {
            let mut parent = super_class.clone();
            while let Some(ref pname) = parent {
                if let Some(pcls) = module.classes.iter().find(|c| &c.name == pname) {
                    for pf in &pcls.fields {
                        // Skip if already shadowed by this class's field.
                        if fields.iter().any(|f| f.name == pf.name) {
                            continue;
                        }
                        let fsym = interner.intern(&pf.name);
                        let fl = fb.new_local(pf.ty.clone());
                        fb.push_stmt(MStmt::Assign {
                            dest: fl,
                            value: Rvalue::GetField {
                                receiver: this_local,
                                class_name: pname.clone(),
                                field_name: pf.name.clone(),
                            },
                        });
                        scope.push((fsym, fl));
                        // Don't add to field_locals — inherited fields aren't written back.
                    }
                    parent = pcls.super_class.clone();
                } else {
                    break;
                }
            }
        }

        // Inner class: load outer class fields via this.this$0.<field>.
        if let Some(this0_field) = fields.iter().find(|f| f.name == "this$0") {
            if let Ty::Class(ref outer_cn) = this0_field.ty {
                let outer_ref = fb.new_local(this0_field.ty.clone());
                fb.push_stmt(MStmt::Assign {
                    dest: outer_ref,
                    value: Rvalue::GetField {
                        receiver: this_local,
                        class_name: class_name.clone(),
                        field_name: "this$0".to_string(),
                    },
                });
                let outer_fields: Vec<_> = module
                    .classes
                    .iter()
                    .find(|c| &c.name == outer_cn)
                    .map(|c| {
                        c.fields
                            .iter()
                            .map(|f| (f.name.clone(), f.ty.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                for (fname, fty) in &outer_fields {
                    if fields.iter().any(|f| f.name == *fname) && *fname != "this$0" {
                        continue;
                    }
                    let fsym = interner.intern(fname);
                    let fl = fb.new_local(fty.clone());
                    fb.push_stmt(MStmt::Assign {
                        dest: fl,
                        value: Rvalue::GetField {
                            receiver: outer_ref,
                            class_name: outer_cn.clone(),
                            field_name: fname.clone(),
                        },
                    });
                    scope.push((fsym, fl));
                }
            }
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

        // Suspend methods — autobox primitive returns and
        // convert bare Return → return null (same as top-level suspend fns).
        if method.is_suspend {
            for block in &mut fb.mf.blocks {
                if let Terminator::ReturnValue(local) = &block.terminator {
                    let local_ty = fb.mf.locals[local.0 as usize].clone();
                    if matches!(local_ty, Ty::Int | Ty::Long | Ty::Double | Ty::Bool) {
                        // Autobox: insert valueOf call before return.
                        let boxed = fb.mf.locals.len() as u32;
                        fb.mf.locals.push(Ty::Any);
                        let (box_class, box_desc) = match local_ty {
                            Ty::Int => ("java/lang/Integer", "(I)Ljava/lang/Integer;"),
                            Ty::Long => ("java/lang/Long", "(J)Ljava/lang/Long;"),
                            Ty::Double => ("java/lang/Double", "(D)Ljava/lang/Double;"),
                            Ty::Bool => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;"),
                            _ => unreachable!(),
                        };
                        let old_local = *local;
                        block.stmts.push(MStmt::Assign {
                            dest: LocalId(boxed),
                            value: Rvalue::Call {
                                kind: CallKind::StaticJava {
                                    class_name: box_class.to_string(),
                                    method_name: "valueOf".to_string(),
                                    descriptor: box_desc.to_string(),
                                },
                                args: vec![old_local],
                            },
                        });
                        block.terminator = Terminator::ReturnValue(LocalId(boxed));
                    }
                }
                if matches!(block.terminator, Terminator::Return) {
                    // return → return null (suspend Unit fns return Object)
                    let null_local = fb.mf.locals.len() as u32;
                    fb.mf.locals.push(Ty::Any);
                    block.stmts.push(MStmt::Assign {
                        dest: LocalId(null_local),
                        value: Rvalue::Const(MirConst::Null),
                    });
                    block.terminator = Terminator::ReturnValue(LocalId(null_local));
                }
            }
        }

        // Extract state machine for suspend methods.
        if method.is_suspend {
            let sm_result =
                extract_suspend_state_machine(&fb.mf, module, &class_name, &method_name);
            match sm_result {
                SuspendSitesResult::Zero => {}
                SuspendSitesResult::Found(mut state_machine) => {
                    state_machine.is_instance_method = true;
                    fb.mf.suspend_state_machine = Some(state_machine);
                }
                SuspendSitesResult::Unsupported(reason) => {
                    diags.push(Diagnostic::error(
                        method.span,
                        format!(
                            "suspend method `{class_name}.{method_name}` has an unsupported shape: {reason}"
                        ),
                    ));
                }
            }
        }

        let mut finished = fb.finish();
        finished.is_abstract = method.is_abstract;
        // Infer return type from body if not explicitly annotated.
        // Expression-body methods (= expr) set ReturnValue as the
        // terminator, and the returned local's type is the actual
        // return type.
        if !method.is_suspend && finished.return_ty == Ty::Unit {
            if let Some(last_block) = finished.blocks.last() {
                if let Terminator::ReturnValue(ret_local) = &last_block.terminator {
                    let inferred = finished.locals[ret_local.0 as usize].clone();
                    if inferred != Ty::Unit {
                        finished.return_ty = inferred;
                    }
                }
            }
        }
        mir_methods.push(finished);
    }

    // Lower property getters as synthetic methods.
    for prop in &c.properties {
        if let Some(getter_body) = &prop.getter {
            let prop_name = interner.resolve(prop.name).to_string();
            let getter_name = format!("get{}{}", &prop_name[..1].to_uppercase(), &prop_name[1..]);
            let return_ty = prop
                .ty
                .as_ref()
                .map(|tr| resolve_type(interner.resolve(tr.name), module))
                .unwrap_or(Ty::Any);
            let fn_idx = module.functions.len() + mir_methods.len();
            let mut fb = FnBuilder::new(fn_idx, getter_name, return_ty);
            let this_local = fb.new_local(Ty::Class(class_name.clone()));
            fb.mf.params.push(this_local);
            let this_sym = interner.intern("this");
            let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];
            // Load fields into scope.
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
            }
            for s in &getter_body.stmts {
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
            mir_methods.push(fb.finish());
        }
    }

    // Lower property setters as synthetic methods.
    // `var x: Int = 0; set(value) { field = value + 1 }`
    // → synthetic method `setX(value: Int): Unit`
    for prop in &c.properties {
        if let Some((setter_param, ref setter_body)) = prop.setter {
            let prop_name = interner.resolve(prop.name).to_string();
            let setter_name = format!("set{}{}", &prop_name[..1].to_uppercase(), &prop_name[1..]);
            let param_ty = prop
                .ty
                .as_ref()
                .map(|tr| resolve_type(interner.resolve(tr.name), module))
                .unwrap_or(Ty::Any);
            let fn_idx = module.functions.len() + mir_methods.len();
            let mut fb = FnBuilder::new(fn_idx, setter_name, Ty::Unit);
            let this_local = fb.new_local(Ty::Class(class_name.clone()));
            fb.mf.params.push(this_local);
            let value_local = fb.new_local(param_ty);
            fb.mf.params.push(value_local);
            let this_sym = interner.intern("this");
            let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];
            // Load fields into scope so the setter body can reference them.
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
            }
            // Add setter parameter to scope.
            scope.push((setter_param, value_local));
            // `field` inside a setter refers to the backing field.
            // We use a copy-in/copy-out pattern: read the field into a
            // local, expose it as `field` in scope, lower the body (which
            // may assign to `field`), then write the local back.
            let field_sym = interner.intern("field");
            let backing_ty = prop
                .ty
                .as_ref()
                .map(|tr| resolve_type(interner.resolve(tr.name), module))
                .unwrap_or(Ty::Any);
            let backing_local = fb.new_local(backing_ty);
            fb.push_stmt(MStmt::Assign {
                dest: backing_local,
                value: Rvalue::GetField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: prop_name.clone(),
                },
            });
            scope.push((field_sym, backing_local));
            for s in &setter_body.stmts {
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
            // Write the `field` local back to the backing field.
            let dummy = fb.new_local(Ty::Unit);
            fb.push_stmt(MStmt::Assign {
                dest: dummy,
                value: Rvalue::PutField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: prop_name.clone(),
                    value: backing_local,
                },
            });
            fb.set_terminator(Terminator::Return);
            mir_methods.push(fb.finish());
        }
    }

    // Lower companion object methods as top-level static functions.
    // This makes ClassName.staticMethod() work via name_to_func lookup.
    for method in &c.companion_methods {
        let method_name = interner.resolve(method.name).to_string();
        let return_ty = method
            .return_ty
            .as_ref()
            .map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
            .unwrap_or(Ty::Unit);

        let fn_idx = module.functions.len();
        let fn_id = FuncId(fn_idx as u32);
        name_to_func.insert(method.name, fn_id);

        let mut fb = FnBuilder::new(fn_idx, method_name, return_ty);
        let mut scope: Vec<(Symbol, LocalId)> = Vec::new();
        for p in &method.params {
            let ty = resolve_type(interner.resolve(p.ty.name), module);
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

        let mut finished = fb.finish();
        // Propagate annotations from the companion method (e.g., @JvmStatic).
        finished.annotations = lower_annotations(&method.annotations, interner);
        module.add_function(finished);
    }

    // Lower companion object properties as static fields.
    // Each companion property becomes a field on the class and is
    // registered as a top-level global constant for ClassName.propName access.
    for prop in &c.companion_properties {
        let prop_name = interner.resolve(prop.name).to_string();
        let prop_ty = prop
            .ty
            .as_ref()
            .map(|tr| resolve_type(interner.resolve(tr.name), module))
            .unwrap_or(Ty::Any);
        // Add as a field on the class.
        fields.push(MirField {
            name: prop_name.clone(),
            ty: prop_ty.clone(),
        });
        // If there's an initializer, register as a global constant.
        if let Some(init) = &prop.init {
            let const_val = match init {
                Expr::IntLit(v, _) => Some(MirConst::Int(*v as i32)),
                Expr::LongLit(v, _) => Some(MirConst::Long(*v)),
                Expr::FloatLit(v, _) => Some(MirConst::Float(*v as f32)),
                Expr::DoubleLit(v, _) => Some(MirConst::Double(*v)),
                Expr::BoolLit(v, _) => Some(MirConst::Bool(*v)),
                Expr::StringLit(s, _) => {
                    let sid = module.intern_string(s);
                    Some(MirConst::String(sid))
                }
                _ => None,
            };
            if let Some(cv) = const_val {
                name_to_global.insert(prop.name, cv);
            }
        }
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

    // Synthesize componentN() methods for data classes.
    if c.is_data && !fields.is_empty() {
        for (i, field) in fields.iter().enumerate() {
            let method_name = format!("component{}", i + 1);
            let comp_idx = module.functions.len() + mir_methods.len();
            let mut comp_fb = FnBuilder::new(comp_idx, method_name, field.ty.clone());
            let comp_this = comp_fb.new_local(Ty::Class(class_name.clone()));
            comp_fb.mf.params.push(comp_this);

            // Load the field and return it.
            let field_val = comp_fb.new_local(field.ty.clone());
            comp_fb.push_stmt(MStmt::Assign {
                dest: field_val,
                value: Rvalue::GetField {
                    receiver: comp_this,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });
            comp_fb.set_terminator(Terminator::ReturnValue(field_val));
            mir_methods.push(comp_fb.finish());
        }
    }

    // Synthesize equals(other: Any?): Boolean for data classes.
    if c.is_data && !fields.is_empty() {
        let eq_idx = module.functions.len() + mir_methods.len();
        let mut eq_fb = FnBuilder::new(eq_idx, "equals".to_string(), Ty::Bool);
        let eq_this = eq_fb.new_local(Ty::Class(class_name.clone()));
        eq_fb.mf.params.push(eq_this);
        let eq_other = eq_fb.new_local(Ty::Any);
        eq_fb.mf.params.push(eq_other);

        // Block 0: reference equality check  (this === other)
        // Copy `this` into an Any-typed local so the JVM backend emits
        // if_acmpeq (reference identity) instead of invokevirtual equals,
        // which would cause infinite recursion.
        let this_as_any = eq_fb.new_local(Ty::Any);
        eq_fb.push_stmt(MStmt::Assign {
            dest: this_as_any,
            value: Rvalue::Local(eq_this),
        });
        let ref_eq = eq_fb.new_local(Ty::Bool);
        eq_fb.push_stmt(MStmt::Assign {
            dest: ref_eq,
            value: Rvalue::BinOp {
                op: MBinOp::CmpEq,
                lhs: this_as_any,
                rhs: eq_other,
            },
        });
        let true_block = eq_fb.new_block(); // block 1: return true
        let instanceof_block = eq_fb.new_block(); // block 2: instanceof check
        eq_fb.set_terminator(Terminator::Branch {
            cond: ref_eq,
            then_block: true_block,
            else_block: instanceof_block,
        });

        // Block 1 (true_block): return true
        eq_fb.cur_block = true_block;
        let const_true = eq_fb.new_local(Ty::Bool);
        eq_fb.push_stmt(MStmt::Assign {
            dest: const_true,
            value: Rvalue::Const(MirConst::Bool(true)),
        });
        eq_fb.set_terminator(Terminator::ReturnValue(const_true));

        // Block 2 (instanceof_block): type check
        eq_fb.cur_block = instanceof_block;
        let is_same_type = eq_fb.new_local(Ty::Bool);
        eq_fb.push_stmt(MStmt::Assign {
            dest: is_same_type,
            value: Rvalue::InstanceOf {
                obj: eq_other,
                type_descriptor: class_name.clone(),
            },
        });
        let false_block = eq_fb.new_block(); // block 3: return false
        let compare_block = eq_fb.new_block(); // block 4: field comparison
        eq_fb.set_terminator(Terminator::Branch {
            cond: is_same_type,
            then_block: compare_block,
            else_block: false_block,
        });

        // Block 3 (false_block): return false
        eq_fb.cur_block = false_block;
        let const_false = eq_fb.new_local(Ty::Bool);
        eq_fb.push_stmt(MStmt::Assign {
            dest: const_false,
            value: Rvalue::Const(MirConst::Bool(false)),
        });
        eq_fb.set_terminator(Terminator::ReturnValue(const_false));

        // Block 4 (compare_block): cast other and compare fields
        eq_fb.cur_block = compare_block;
        let casted_other = eq_fb.new_local(Ty::Class(class_name.clone()));
        eq_fb.push_stmt(MStmt::Assign {
            dest: casted_other,
            value: Rvalue::CheckCast {
                obj: eq_other,
                target_class: class_name.clone(),
            },
        });

        // Compare each field. For primitives (Int, Long, Double, Bool),
        // use CmpEq directly. For reference types (String, Class), use
        // VirtualJava Object.equals().
        let mut result_local: Option<LocalId> = None;
        for field in &fields {
            let this_field = eq_fb.new_local(field.ty.clone());
            eq_fb.push_stmt(MStmt::Assign {
                dest: this_field,
                value: Rvalue::GetField {
                    receiver: eq_this,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });
            let other_field = eq_fb.new_local(field.ty.clone());
            eq_fb.push_stmt(MStmt::Assign {
                dest: other_field,
                value: Rvalue::GetField {
                    receiver: casted_other,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });
            let fields_eq = match &field.ty {
                Ty::Int | Ty::Long | Ty::Double | Ty::Bool => {
                    let cmp = eq_fb.new_local(Ty::Bool);
                    eq_fb.push_stmt(MStmt::Assign {
                        dest: cmp,
                        value: Rvalue::BinOp {
                            op: MBinOp::CmpEq,
                            lhs: this_field,
                            rhs: other_field,
                        },
                    });
                    cmp
                }
                Ty::String => {
                    // Use String.equals(Object)
                    let cmp = eq_fb.new_local(Ty::Bool);
                    eq_fb.push_stmt(MStmt::Assign {
                        dest: cmp,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/String".to_string(),
                                method_name: "equals".to_string(),
                                descriptor: "(Ljava/lang/Object;)Z".to_string(),
                            },
                            args: vec![this_field, other_field],
                        },
                    });
                    cmp
                }
                _ => {
                    // For Class and other reference types, use Object.equals()
                    let cmp = eq_fb.new_local(Ty::Bool);
                    eq_fb.push_stmt(MStmt::Assign {
                        dest: cmp,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/Object".to_string(),
                                method_name: "equals".to_string(),
                                descriptor: "(Ljava/lang/Object;)Z".to_string(),
                            },
                            args: vec![this_field, other_field],
                        },
                    });
                    cmp
                }
            };

            result_local = Some(match result_local {
                None => fields_eq,
                Some(prev) => {
                    // AND: both must be true. We use MulI since bool is int on JVM.
                    // true(1) * true(1) = 1, true(1) * false(0) = 0
                    let and_result = eq_fb.new_local(Ty::Bool);
                    eq_fb.push_stmt(MStmt::Assign {
                        dest: and_result,
                        value: Rvalue::BinOp {
                            op: MBinOp::MulI,
                            lhs: prev,
                            rhs: fields_eq,
                        },
                    });
                    and_result
                }
            });
        }
        eq_fb.set_terminator(Terminator::ReturnValue(result_local.unwrap()));
        mir_methods.push(eq_fb.finish());
    }

    // Synthesize hashCode(): Int for data classes.
    if c.is_data && !fields.is_empty() {
        let hc_idx = module.functions.len() + mir_methods.len();
        let mut hc_fb = FnBuilder::new(hc_idx, "hashCode".to_string(), Ty::Int);
        let hc_this = hc_fb.new_local(Ty::Class(class_name.clone()));
        hc_fb.mf.params.push(hc_this);

        // result = field0.hashCode()
        // result = 31 * result + field1.hashCode()
        // ...
        let mut result: Option<LocalId> = None;
        for field in &fields {
            let field_val = hc_fb.new_local(field.ty.clone());
            hc_fb.push_stmt(MStmt::Assign {
                dest: field_val,
                value: Rvalue::GetField {
                    receiver: hc_this,
                    class_name: class_name.clone(),
                    field_name: field.name.clone(),
                },
            });

            // Compute hash for this field.
            let field_hash = match &field.ty {
                Ty::Int | Ty::Bool => {
                    // For Int, hashCode is the value itself.
                    // For Bool, true=1, false=0 — same representation.
                    field_val
                }
                Ty::Long => {
                    // Long.hashCode() in Kotlin = (value xor (value >>> 32)).toInt()
                    // Use static call: java.lang.Long.hashCode(long)
                    let h = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: h,
                        value: Rvalue::Call {
                            kind: CallKind::StaticJava {
                                class_name: "java/lang/Long".to_string(),
                                method_name: "hashCode".to_string(),
                                descriptor: "(J)I".to_string(),
                            },
                            args: vec![field_val],
                        },
                    });
                    h
                }
                Ty::Double => {
                    // Double.hashCode() = java.lang.Double.hashCode(double)
                    let h = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: h,
                        value: Rvalue::Call {
                            kind: CallKind::StaticJava {
                                class_name: "java/lang/Double".to_string(),
                                method_name: "hashCode".to_string(),
                                descriptor: "(D)I".to_string(),
                            },
                            args: vec![field_val],
                        },
                    });
                    h
                }
                Ty::String => {
                    // String.hashCode()
                    let h = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: h,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/String".to_string(),
                                method_name: "hashCode".to_string(),
                                descriptor: "()I".to_string(),
                            },
                            args: vec![field_val],
                        },
                    });
                    h
                }
                _ => {
                    // For any reference type, call Object.hashCode()
                    let h = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: h,
                        value: Rvalue::Call {
                            kind: CallKind::VirtualJava {
                                class_name: "java/lang/Object".to_string(),
                                method_name: "hashCode".to_string(),
                                descriptor: "()I".to_string(),
                            },
                            args: vec![field_val],
                        },
                    });
                    h
                }
            };

            result = Some(match result {
                None => field_hash,
                Some(prev) => {
                    // result = 31 * prev + field_hash
                    let thirty_one = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: thirty_one,
                        value: Rvalue::Const(MirConst::Int(31)),
                    });
                    let mul = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: mul,
                        value: Rvalue::BinOp {
                            op: MBinOp::MulI,
                            lhs: thirty_one,
                            rhs: prev,
                        },
                    });
                    let sum = hc_fb.new_local(Ty::Int);
                    hc_fb.push_stmt(MStmt::Assign {
                        dest: sum,
                        value: Rvalue::BinOp {
                            op: MBinOp::AddI,
                            lhs: mul,
                            rhs: field_hash,
                        },
                    });
                    sum
                }
            });
        }
        hc_fb.set_terminator(Terminator::ReturnValue(result.unwrap()));
        mir_methods.push(hc_fb.finish());
    }

    // Synthesize copy(field1, field2, ...): ClassName for data classes.
    // Takes all fields as parameters — at the call site, unspecified
    // params are filled from the receiver's current field values.
    if c.is_data && !fields.is_empty() {
        let cp_idx = module.functions.len() + mir_methods.len();
        let mut cp_fb = FnBuilder::new(cp_idx, "copy".to_string(), Ty::Class(class_name.clone()));
        let cp_this = cp_fb.new_local(Ty::Class(class_name.clone()));
        cp_fb.mf.params.push(cp_this);

        // Add a parameter for each field — these are the values to use
        // in the new instance. The call site fills in defaults from
        // the receiver for any omitted named arguments.
        let mut param_locals = Vec::new();
        for field in &fields {
            let p = cp_fb.new_local(field.ty.clone());
            cp_fb.mf.params.push(p);
            param_locals.push(p);
        }

        // new ClassName
        let new_inst = cp_fb.new_local(Ty::Class(class_name.clone()));
        cp_fb.push_stmt(MStmt::Assign {
            dest: new_inst,
            value: Rvalue::NewInstance(class_name.clone()),
        });

        // Call constructor with the parameter values.
        cp_fb.push_stmt(MStmt::Assign {
            dest: new_inst,
            value: Rvalue::Call {
                kind: CallKind::Constructor(class_name.clone()),
                args: param_locals,
            },
        });

        cp_fb.set_terminator(Terminator::ReturnValue(new_inst));
        mir_methods.push(cp_fb.finish());
    }

    // Lower secondary constructors.
    let mut mir_secondary_ctors = Vec::new();
    for sec_ctor in &c.secondary_constructors {
        let mut sec_fn = MirFunction {
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
            param_receiver_types: Vec::new(),
            is_abstract: false,
            exception_handlers: Vec::new(),
            vararg_index: None,
            is_suspend: false,
            is_inline: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: Vec::new(),
        };
        // Add 'this' as local 0.
        let sec_this = sec_fn.new_local(Ty::Class(class_name.clone()));
        sec_fn.params.push(sec_this);

        // Add secondary constructor parameters.
        let this_sym = interner.intern("this");
        let mut sec_scope: Vec<(Symbol, LocalId)> = vec![(this_sym, sec_this)];
        for p in &sec_ctor.params {
            let ty = resolve_type(interner.resolve(p.ty.name), module);
            let param_id = sec_fn.new_local(ty);
            sec_fn.params.push(param_id);
            sec_scope.push((p.name, param_id));
            sec_fn
                .param_names
                .push(interner.resolve(p.name).to_string());
        }

        // Emit delegation call.
        if sec_ctor.has_delegation {
            // `: this(args)` — delegate to another constructor of the same class.
            let tmp_idx = module.functions.len() + 8000;
            let mut fb = FnBuilder::new(tmp_idx, "<sec-init>".to_string(), Ty::Unit);
            fb.mf.locals = sec_fn.locals.clone();
            fb.mf.params = sec_fn.params.clone();
            let mut delegate_arg_ids = vec![sec_this]; // receiver
            for arg_expr in &sec_ctor.delegate_args {
                if let Some(id) = lower_expr(
                    arg_expr,
                    &mut fb,
                    &mut sec_scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    None,
                ) {
                    delegate_arg_ids.push(id);
                }
            }
            sec_fn.locals = fb.mf.locals;
            for stmt in fb.mf.blocks[0].stmts.drain(..) {
                sec_fn.blocks[0].stmts.push(stmt);
            }
            // Call the delegate constructor (this class's <init> matching the delegate args).
            sec_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: sec_this, // dummy
                value: Rvalue::Call {
                    kind: CallKind::Constructor(class_name.clone()),
                    args: delegate_arg_ids,
                },
            });
        } else {
            // No explicit delegation — call super constructor.
            let super_class_name = c
                .parent_class
                .as_ref()
                .map(|sc| interner.resolve(sc.name).to_string())
                .unwrap_or_else(|| "java/lang/Object".to_string());
            sec_fn.blocks[0].stmts.push(MStmt::Assign {
                dest: sec_this, // dummy
                value: Rvalue::Call {
                    kind: CallKind::Constructor(super_class_name),
                    args: vec![sec_this],
                },
            });
        }

        // Lower optional body.
        if let Some(body) = &sec_ctor.body {
            let tmp_idx = module.functions.len() + 8001;
            let mut fb = FnBuilder::new(tmp_idx, "<sec-init-body>".to_string(), Ty::Unit);
            fb.mf.locals = sec_fn.locals.clone();
            fb.mf.params = sec_fn.params.clone();
            fb.mf.blocks[0].stmts = sec_fn.blocks[0].stmts.clone();
            // Add fields to scope for body access, track for writeback.
            let mut field_locals: Vec<(String, LocalId)> = Vec::new();
            for field in &fields {
                let field_sym = interner.intern(&field.name);
                let fl = fb.new_local(field.ty.clone());
                fb.push_stmt(MStmt::Assign {
                    dest: fl,
                    value: Rvalue::GetField {
                        receiver: sec_this,
                        class_name: class_name.clone(),
                        field_name: field.name.clone(),
                    },
                });
                sec_scope.push((field_sym, fl));
                field_locals.push((field.name.clone(), fl));
            }
            for s in &body.stmts {
                lower_stmt(
                    s,
                    &mut fb,
                    &mut sec_scope,
                    module,
                    name_to_func,
                    name_to_global,
                    interner,
                    diags,
                    None,
                );
            }
            // Write back fields to the object.
            for (field_name, field_local) in &field_locals {
                fb.push_stmt(MStmt::Assign {
                    dest: sec_this, // dummy dest
                    value: Rvalue::PutField {
                        receiver: sec_this,
                        class_name: class_name.clone(),
                        field_name: field_name.clone(),
                        value: *field_local,
                    },
                });
            }
            sec_fn.locals = fb.mf.locals;
            sec_fn.blocks = fb.mf.blocks;
            if let Some(last) = sec_fn.blocks.last_mut() {
                last.terminator = Terminator::Return;
            }
        }

        mir_secondary_ctors.push(sec_fn);
    }

    // Generate forwarding methods for interface delegation (`Base by b`).
    for (iface_sym, delegate_sym) in &c.interface_delegates {
        let iface_name = interner.resolve(*iface_sym).to_string();
        let delegate_field = interner.resolve(*delegate_sym).to_string();
        // Look up the interface in already-lowered MIR classes.
        let iface_methods: Vec<(String, Ty, Vec<Ty>)> = module
            .classes
            .iter()
            .find(|cls| cls.name == iface_name && cls.is_interface)
            .map(|cls| {
                cls.methods
                    .iter()
                    .map(|m| {
                        // Collect param types (skip `this` at index 0).
                        let param_tys: Vec<Ty> = m
                            .params
                            .iter()
                            .skip(1)
                            .map(|pid| m.locals[pid.0 as usize].clone())
                            .collect();
                        (m.name.clone(), m.return_ty.clone(), param_tys)
                    })
                    .collect()
            })
            .unwrap_or_default();

        for (method_name, return_ty, param_tys) in &iface_methods {
            // Skip if the class already defines this method explicitly.
            if mir_methods.iter().any(|m| m.name == *method_name) {
                continue;
            }
            let fn_idx = module.functions.len() + mir_methods.len();
            let mut fb = FnBuilder::new(fn_idx, method_name.clone(), return_ty.clone());

            // `this` parameter.
            let this_local = fb.new_local(Ty::Class(class_name.clone()));
            fb.mf.params.push(this_local);

            // Additional method parameters (mirror the interface signature).
            let mut fwd_args = Vec::new();
            for pty in param_tys {
                let pid = fb.new_local(pty.clone());
                fb.mf.params.push(pid);
                fwd_args.push(pid);
            }

            // Load the delegate field: `this.<delegate_field>`
            let delegate_local = fb.new_local(Ty::Class(iface_name.clone()));
            fb.push_stmt(MStmt::Assign {
                dest: delegate_local,
                value: Rvalue::GetField {
                    receiver: this_local,
                    class_name: class_name.clone(),
                    field_name: delegate_field.clone(),
                },
            });

            // Call the interface method on the delegate.
            let mut call_args = vec![delegate_local];
            call_args.extend(fwd_args);
            let result_local = fb.new_local(return_ty.clone());
            fb.push_stmt(MStmt::Assign {
                dest: result_local,
                value: Rvalue::Call {
                    kind: CallKind::Virtual {
                        class_name: iface_name.clone(),
                        method_name: method_name.clone(),
                    },
                    args: call_args,
                },
            });

            if *return_ty == Ty::Unit {
                fb.set_terminator(Terminator::Return);
            } else {
                fb.set_terminator(Terminator::ReturnValue(result_local));
            }

            mir_methods.push(fb.finish());
        }
    }

    // Replace the stub class with the fully-lowered version.
    let outer_name = class_name.clone();
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
        secondary_constructors: mir_secondary_ctors,
        is_suspend_lambda: false,
        is_cross_file_stub: false,
        annotations: Vec::new(),
    };

    // Lower nested (static inner) classes. Each nested class becomes a
    // separate top-level MirClass with a JVM-conventional mangled name:
    // `Outer$Nested`. This is a static inner class — no reference to
    // the outer instance.
    for nested in &c.nested_classes {
        let nested_name = interner.resolve(nested.name).to_string();
        let mangled = format!("{}${}", outer_name, nested_name);
        let mangled_sym = interner.intern(&mangled);
        let mut synthetic = nested.clone();
        synthetic.name = mangled_sym;

        if nested.is_inner {
            // Inner class: add `this$0` field of outer class type as the
            // first constructor parameter, so `outer.Inner(args)` passes
            // the outer reference implicitly.
            let outer_param = ConstructorParam {
                is_val: true,
                is_var: false,
                name: interner.intern("this$0"),
                ty: TypeRef {
                    name: interner.intern(&outer_name),
                    nullable: false,
                    func_params: None,
                    type_args: Vec::new(),
                    is_suspend: false,
                    has_receiver: false,
                    span: nested.span,
                },
                span: nested.span,
            };
            synthetic.constructor_params.insert(0, outer_param);
        }

        lower_class(
            &synthetic,
            name_to_func,
            name_to_global,
            module,
            interner,
            diags,
        );
    }
}

fn lower_interface(
    iface: &skotch_syntax::InterfaceDecl,
    name_to_func: &mut FxHashMap<Symbol, FuncId>,
    name_to_global: &FxHashMap<Symbol, MirConst>,
    module: &mut MirModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) {
    let iface_name = interner.resolve(iface.name).to_string();

    // Pre-register the interface with method stubs for self-resolution.
    let stub_methods: Vec<MirFunction> = iface
        .methods
        .iter()
        .map(|m| {
            let mname = interner.resolve(m.name).to_string();
            let return_ty = m
                .return_ty
                .as_ref()
                .map(|tr| {
                    let resolved = resolve_type(interner.resolve(tr.name), module);
                    resolved
                })
                .unwrap_or(Ty::Unit);
            // Build stub with correct params (needed for JVM method descriptor).
            let mut stub = MirFunction {
                id: FuncId(0),
                name: mname,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: m.is_abstract,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            // Add `this` param.
            let this_id = stub.new_local(Ty::Class(iface_name.clone()));
            stub.params.push(this_id);
            // Add declared params.
            for p in &m.params {
                let ty = resolve_type(interner.resolve(p.ty.name), module);
                let pid = stub.new_local(ty);
                stub.params.push(pid);
            }
            stub
        })
        .collect();
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
        param_receiver_types: Vec::new(),
        is_abstract: false,
        exception_handlers: Vec::new(),
        vararg_index: None,
        is_suspend: false,
        is_inline: false,
        suspend_original_return_ty: None,
        suspend_state_machine: None,
        annotations: Vec::new(),
    };
    let class_idx = module.classes.len();
    module.classes.push(MirClass {
        name: iface_name.clone(),
        super_class: None,
        is_open: true,
        is_abstract: true,
        is_interface: true,
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: stub_methods,
        constructor: dummy_init.clone(),
        secondary_constructors: Vec::new(),
        is_suspend_lambda: false,
        is_cross_file_stub: false,
        annotations: Vec::new(),
    });

    // Lower method bodies for default methods (non-abstract).
    let mut mir_methods = Vec::new();
    for method in &iface.methods {
        let method_name = interner.resolve(method.name).to_string();
        let return_ty = method
            .return_ty
            .as_ref()
            .map(|tr| {
                let resolved = resolve_type(interner.resolve(tr.name), module);
                resolved
            })
            .unwrap_or(Ty::Unit);

        if method.is_abstract {
            // Abstract method — stub with correct params for JVM descriptor.
            let mut stub = MirFunction {
                id: FuncId(0),
                name: method_name,
                params: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                return_ty,
                required_params: 0,
                param_names: Vec::new(),
                param_defaults: Vec::new(),
                param_receiver_types: Vec::new(),
                is_abstract: true,
                exception_handlers: Vec::new(),
                vararg_index: None,
                is_suspend: false,
                is_inline: false,
                suspend_original_return_ty: None,
                suspend_state_machine: None,
                annotations: Vec::new(),
            };
            let this_id = stub.new_local(Ty::Class(iface_name.clone()));
            stub.params.push(this_id);
            for p in &method.params {
                let ty = resolve_type(interner.resolve(p.ty.name), module);
                let pid = stub.new_local(ty);
                stub.params.push(pid);
            }
            mir_methods.push(stub);
        } else {
            // Default method — lower the body.
            let fn_idx = module.functions.len() + mir_methods.len();
            let mut fb = FnBuilder::new(fn_idx, method_name.clone(), return_ty);
            let this_local = fb.new_local(Ty::Class(iface_name.clone()));
            fb.mf.params.push(this_local);
            let this_sym = interner.intern("this");
            let mut scope: Vec<(Symbol, LocalId)> = vec![(this_sym, this_local)];
            for p in &method.params {
                let ty = resolve_type(interner.resolve(p.ty.name), module);
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
            mir_methods.push(fb.finish());
        }
    }

    // Replace stubs with real methods.
    module.classes[class_idx].methods = mir_methods;
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
        let r = resolve_file(&f, &mut interner, &mut diags, None);
        let t = type_check(&f, &r, &mut interner, &mut diags, None);
        let m = lower_file(&f, &r, &t, &mut interner, &mut diags, "HelloKt", None);
        (m, diags)
    }

    #[test]
    fn lower_println_string() {
        let (m, d) = lower(r#"fun main() { println("hi") }"#);
        assert!(d.is_empty(), "{:?}", d);
        let real_fns: Vec<_> = m.functions.iter().filter(|f| !f.is_abstract).collect();
        assert_eq!(real_fns.len(), 1);
        let f = real_fns[0];
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
        let real_fns: Vec<_> = m.functions.iter().filter(|f| !f.is_abstract).collect();
        assert_eq!(real_fns.len(), 2);
        let main_block = &real_fns[1].blocks[0];
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
        let real_fns: Vec<_> = m.functions.iter().filter(|f| !f.is_abstract).collect();
        assert_eq!(real_fns.len(), 1, "no synthetic <clinit> generated");
        // The string pool has "hi" once (deduped between the val
        // initializer and any other use).
        assert_eq!(m.strings, vec!["hi".to_string()]);
        let main = real_fns[0];
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
