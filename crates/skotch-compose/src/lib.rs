//! Compose Compiler Plugin — `@Composable` function transform.
//!
//! This crate implements the core Compose compiler transformation as a
//! MIR-to-MIR pass, similar in architecture to the coroutine CPS transform
//! in `skotch-mir-lower`.
//!
//! # What the transform does
//!
//! For each `@Composable` function:
//! 1. Inject a `$composer: Composer` parameter (first position)
//! 2. Inject a `$changed: Int` bitmask parameter (last position)
//! 3. Wrap the function body in `composer.startRestartGroup(key)` /
//!    `composer.endRestartGroup()?.updateScope { ... }`
//! 4. Generate unique group keys for positional memoization
//!
//! # Architecture
//!
//! ```text
//! MirModule (pre-transform)
//!     │
//!     ▼
//! compose_transform(module) ← this crate
//!     │
//!     ▼
//! MirModule (post-transform, with Composer params injected)
//! ```
//!
//! The transform runs AFTER MIR lowering and BEFORE backend emission.
//! It only affects functions annotated with `@Composable` (detected via
//! `MirFunction.annotations`).

use skotch_mir::{
    CallKind, FuncId, LocalId, MirConst, MirFunction, MirModule, Rvalue, Stmt as MStmt,
};
use skotch_types::Ty;

/// Compute a stable group key for a composable function. kotlinc uses a
/// Murmur3-style hash of the function's source location ("`<name>
/// (<file>:<line>)`"); we don't have line info at this layer, so we
/// stabilize on a simpler FNV-1a hash of the function name. That keeps
/// the emitted bytecode deterministic across runs without depending on
/// the order fixtures are generated in.
fn group_key_for(name: &str) -> i32 {
    let mut h: u32 = 0x811C9DC5;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h as i32
}

/// Apply the Compose transform to all `@Composable` functions in a module.
///
/// This is the main entry point. Call after MIR lowering, before backend.
pub fn compose_transform(module: &mut MirModule) {
    let composable_fids: Vec<FuncId> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| is_composable(f))
        .map(|(i, _)| FuncId(i as u32))
        .collect();

    for fid in composable_fids {
        transform_composable_function(&mut module.functions[fid.0 as usize]);
    }

    // Also transform composable methods in classes.
    for class in &mut module.classes {
        let composable_methods: Vec<usize> = class
            .methods
            .iter()
            .enumerate()
            .filter(|(_, m)| is_composable(m))
            .map(|(i, _)| i)
            .collect();
        for idx in composable_methods {
            transform_composable_function(&mut class.methods[idx]);
        }
    }

    // After transforming composable functions (which adds $composer/$changed
    // params), update ALL call sites in the module that call composable
    // functions via CallKind::Static. These calls were lowered before the
    // transform and have the original arg count — they need the extra args.
    //
    // We record each composable function's POST-transform param count so
    // call-site patching can fill the right number of missing slots
    // (user defaults + $composer + $changed + optional $default mask). The
    // simple "+2" approximation in the original transform fails for
    // composables with default params, where the gap between caller args
    // and callee params can be 3+ (e.g. `JetchatTheme { ... }` calls a 5-
    // param signature with just 1 arg — 4 missing).
    let composable_param_types: std::collections::HashMap<u32, Vec<Ty>> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| is_composable(f))
        .map(|(i, f)| {
            // params hold local IDs; resolve to the function's local types.
            let tys: Vec<Ty> = f
                .params
                .iter()
                .map(|lid| f.locals.get(lid.0 as usize).cloned().unwrap_or(Ty::Any))
                .collect();
            (i as u32, tys)
        })
        .collect();
    // Patch lambda classes that serve as @Composable content blocks
    // FIRST — this adds `$composer: Composer` and `$changed: Int` to
    // each lambda's `invoke` method's params. The subsequent call-site
    // patching relies on the enclosing function/method already having
    // a `Composer` param to thread through; without this ordering, calls
    // inside a `@Composable` lambda body (e.g. `rememberDrawerState`)
    // get a placeholder `null` for the `$composer` argument, which then
    // throws NPE inside the Compose runtime as soon as the composition
    // executes. The Compose runtime expects @Composable lambdas to
    // implement Function2<Composer, Int, Unit> (or Function3 for
    // lambdas with one user parameter, etc.) instead of the plain
    // FunctionN; we detect lambda classes whose invoke method has the
    // @Composable annotation OR whose interface is Function0 and
    // they're used in a composable context — and bump their arity by 2.
    patch_composable_lambda_interfaces(module, &composable_param_types);
    patch_static_calls_to_composable(module, &composable_param_types);
}

/// Patch lambda classes to implement the correct FunctionN interface
/// for Compose. A @Composable () -> Unit lambda must implement
/// Function2<Composer, Int, Unit> instead of Function0<Unit>.
fn patch_composable_lambda_interfaces(
    module: &mut MirModule,
    composable_param_types: &std::collections::HashMap<u32, Vec<Ty>>,
) {
    // Build `lambda_use_arity`: for every Function0 lambda class that's
    // passed at a `@Composable () -> Unit` (or higher-arity composable
    // lambda) arg position of some composable call, record the
    // FunctionN arity it ought to implement. This catches lambdas whose
    // own invoke body has no Composer-mentioning calls (e.g. JetChat's
    // JetchatScaffoldKt$Lambda$19 — its invoke just wraps a call to
    // `ModalNavigationDrawer` whose MIR descriptor doesn't currently
    // expose the trailing `Composer;I` params) but which are still
    // required to be Function2 by their caller's declared signature.
    let mut lambda_use_arity: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut record_lambda_use = |arg_local_ty: &Ty, param_ty: &Ty| {
        let Ty::Class(lambda_name) = arg_local_ty else {
            return;
        };
        if !lambda_name.contains("$Lambda$") {
            return;
        }
        let Ty::Function {
            params,
            is_composable,
            is_suspend,
            ..
        } = param_ty
        else {
            return;
        };
        if !*is_composable && !*is_suspend {
            return;
        }
        let arity =
            params.len() + if *is_composable { 2 } else { 0 } + if *is_suspend { 1 } else { 0 };
        let entry = lambda_use_arity.entry(lambda_name.clone()).or_insert(0);
        if arity > *entry {
            *entry = arity;
        }
    };
    // Parse a JVM method descriptor's parameter list into a Vec of
    // descriptor strings. Used to detect FunctionN positions in
    // CallKind::StaticJava descriptors (e.g. `(ZZLkotlin/jvm/functions/Function2;Landroidx/compose/runtime/Composer;I)V`).
    fn parse_descriptor_params(desc: &str) -> Vec<String> {
        let mut params = Vec::new();
        let inside = desc
            .strip_prefix('(')
            .and_then(|s| s.split(')').next())
            .unwrap_or("");
        let mut chars = inside.chars().peekable();
        while let Some(c) = chars.next() {
            let mut p = String::new();
            p.push(c);
            if c == '[' {
                while chars.peek() == Some(&'[') {
                    p.push(chars.next().unwrap());
                }
                if let Some(&next) = chars.peek() {
                    if next == 'L' {
                        p.push(chars.next().unwrap());
                        for ch in chars.by_ref() {
                            p.push(ch);
                            if ch == ';' {
                                break;
                            }
                        }
                    } else {
                        p.push(chars.next().unwrap());
                    }
                }
            } else if c == 'L' {
                for ch in chars.by_ref() {
                    p.push(ch);
                    if ch == ';' {
                        break;
                    }
                }
            }
            params.push(p);
        }
        params
    }
    let scan_body = |func: &MirFunction,
                     locals_provider: &dyn Fn(LocalId) -> Ty,
                     mut on_use: &mut dyn FnMut(&Ty, &Ty)| {
        for block in &func.blocks {
            for stmt in &block.stmts {
                let MStmt::Assign { value, .. } = stmt;
                match value {
                    Rvalue::Call {
                        kind: CallKind::Static(fid),
                        args,
                    } => {
                        let Some(param_tys) = composable_param_types.get(&fid.0) else {
                            continue;
                        };
                        for (i, arg) in args.iter().enumerate() {
                            let Some(param_ty) = param_tys.get(i) else {
                                continue;
                            };
                            let arg_ty = locals_provider(*arg);
                            on_use(&arg_ty, param_ty);
                        }
                    }
                    Rvalue::Call {
                        kind: CallKind::StaticJava { descriptor, .. },
                        args,
                    } => {
                        // Use the descriptor's FunctionN annotations as a
                        // fallback signal — JVM-erased but still useful:
                        // any `Lkotlin/jvm/functions/FunctionN;` at a
                        // composer-following slot indicates a composable
                        // callback whose arity is N (without Composer +
                        // Int).
                        let dparams = parse_descriptor_params(descriptor);
                        let has_composer_param = dparams.iter().any(|p| p.contains("Composer;"));
                        if !has_composer_param {
                            continue;
                        }
                        for (i, arg) in args.iter().enumerate() {
                            let Some(dp) = dparams.get(i) else {
                                continue;
                            };
                            let arg_ty = locals_provider(*arg);
                            // Synthesize a Ty::Function { is_composable:
                            // true } when the descriptor slot is
                            // FunctionN and the call also passes a
                            // Composer somewhere — that's the contract
                            // kotlinc emits for `@Composable` lambda
                            // params.
                            let Some(fn_arity_str) =
                                dp.strip_prefix("Lkotlin/jvm/functions/Function")
                            else {
                                continue;
                            };
                            let fn_arity_str = fn_arity_str.trim_end_matches(';');
                            let Ok(n) = fn_arity_str.parse::<usize>() else {
                                continue;
                            };
                            if n < 2 {
                                continue;
                            }
                            let synth = Ty::Function {
                                params: vec![Ty::Any; n.saturating_sub(2)],
                                ret: Box::new(Ty::Unit),
                                is_suspend: false,
                                is_composable: true,
                            };
                            on_use(&arg_ty, &synth);
                        }
                    }
                    _ => {}
                }
            }
        }
        // Silence "unused" — the body is called via the closure below.
        let _ = &mut on_use;
    };
    for func in &module.functions {
        scan_body(
            func,
            &|lid: LocalId| func.locals[lid.0 as usize].clone(),
            &mut record_lambda_use,
        );
    }
    for class in &module.classes {
        for method in &class.methods {
            scan_body(
                method,
                &|lid: LocalId| method.locals[lid.0 as usize].clone(),
                &mut record_lambda_use,
            );
        }
    }

    // Bump Function0 lambda classes to Function2+ either when the
    // lambda's invoke body makes Composer-mentioning calls OR when the
    // lambda is passed at a composable arg position elsewhere in the
    // module. Non-composable Function0 lambdas (e.g. the calculation
    // block passed to `remember { ... }`) must STAY Function0 — bumping
    // them breaks call-site casts like `checkcast Function0` that the
    // surrounding code performs before invoking.
    for class in &mut module.classes {
        if !class.name.contains("$Lambda$") {
            continue;
        }
        let has_function0 = class
            .interfaces
            .iter()
            .any(|i| i == "kotlin/jvm/functions/Function0");
        if !has_function0 {
            continue;
        }
        // Detect composable-ness by scanning the invoke body for
        // Composer-mentioning call descriptors.
        let invoke_has_composer_call = class
            .methods
            .iter()
            .filter(|m| m.name == "invoke")
            .flat_map(|m| m.blocks.iter())
            .flat_map(|b| b.stmts.iter())
            .any(|stmt| {
                let MStmt::Assign { value, .. } = stmt;
                match value {
                    Rvalue::Call {
                        kind: CallKind::StaticJava { descriptor, .. },
                        ..
                    }
                    | Rvalue::Call {
                        kind: CallKind::VirtualJava { descriptor, .. },
                        ..
                    } => descriptor.contains("Composer;"),
                    _ => false,
                }
            });
        // Fallback: if the lambda is passed at a composable param
        // position somewhere in the module, the caller's declared
        // signature dictates the lambda's interface even when the
        // invoke body's own calls don't reveal it.
        let use_arity = lambda_use_arity.get(&class.name).copied();
        if !invoke_has_composer_call && use_arity.is_none() {
            continue;
        }
        for iface in &mut class.interfaces {
            if let Some(n_str) = iface.strip_prefix("kotlin/jvm/functions/Function") {
                if let Ok(n) = n_str.parse::<usize>() {
                    *iface = format!("kotlin/jvm/functions/Function{}", n + 2);
                }
            }
        }
        // Also inject $composer and $changed into the invoke method's
        // MIR params so that inner composable function calls can forward
        // the composer. The params are added AFTER existing ones — they
        // map to JVM slots after `this` + captured-field slots.
        for method in &mut class.methods {
            if method.name == "invoke" {
                // Add $composer param as Composer — the typed invoke descriptor
                // uses the Composer class directly. The bridge handles Object→Composer.
                let composer_id = LocalId(method.locals.len() as u32);
                method
                    .locals
                    .push(Ty::Class("androidx/compose/runtime/Composer".to_string()));
                method.params.push(composer_id);
                method.param_names.push("$composer".to_string());

                // Add $changed param as Ty::Int — the typed invoke descriptor
                // uses int directly, matching how the body uses $changed.
                // The bridge invoke(Object, Object)Object handles the
                // Object→int unboxing for the FunctionN interface.
                let changed_id = LocalId(method.locals.len() as u32);
                method.locals.push(Ty::Int);
                method.params.push(changed_id);
                method.param_names.push("$changed".to_string());

                // The composable lambda's invoke returns Unit (void) — the
                // bridge invoke(Object, Object)Object handles wrapping
                // void into a kotlin/Unit.INSTANCE return. The MIR lowerer
                // sized the method as Function0.invoke():Object so update
                // the return_ty here. Body terminators that return null
                // are rewritten to plain returns below.
                method.return_ty = Ty::Unit;

                // Rewrite any `ReturnValue(local)` terminators where the
                // local is a Const(Null) into a plain `Return` so the body
                // matches the now-void signature.
                let null_locals: std::collections::HashSet<u32> = method
                    .blocks
                    .iter()
                    .flat_map(|b| b.stmts.iter())
                    .filter_map(|stmt| {
                        let MStmt::Assign { dest, value } = stmt;
                        matches!(value, Rvalue::Const(MirConst::Null)).then_some(dest.0)
                    })
                    .collect();
                for block in &mut method.blocks {
                    if let skotch_mir::Terminator::ReturnValue(lid) = &block.terminator {
                        if null_locals.contains(&lid.0) {
                            block.terminator = skotch_mir::Terminator::Return;
                        }
                    }
                }

                // Thread the new composer/changed params through composable
                // call sites inside the body. MIR lowering ran before these
                // params existed, so those calls got null/0 placeholders at
                // the Composer / $changed positions. Use the descriptor to
                // locate the right slots and rewrite the placeholders.
                thread_composer_args(method, composer_id, changed_id);
            }
        }
    }
}

/// Replace null $composer and zero $changed placeholders in composable
/// function calls with references to the actual lambda invoke params.
/// Uses the call's JVM descriptor to find the exact Composer position
/// (and the adjacent $changed slot), then rewrites those args if they
/// hold the placeholder constants emitted by MIR lowering.
fn thread_composer_args(func: &mut MirFunction, composer_local: LocalId, changed_local: LocalId) {
    use skotch_mir::MirConst;

    // Locals that hold a literal Const(Null) — these are the placeholders
    // emitted by the MIR lowering for the $composer slot.
    let null_locals: std::collections::HashSet<u32> = func
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| {
            let MStmt::Assign { dest, value } = stmt;
            if matches!(value, Rvalue::Const(MirConst::Null)) {
                Some(dest.0)
            } else {
                None
            }
        })
        .collect();

    // Locals assigned Const(Int(0)) — placeholders for $changed.
    let zero_locals: std::collections::HashSet<u32> = func
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| {
            let MStmt::Assign { dest, value } = stmt;
            if matches!(value, Rvalue::Const(MirConst::Int(0))) {
                Some(dest.0)
            } else {
                None
            }
        })
        .collect();

    for block in &mut func.blocks {
        for stmt in &mut block.stmts {
            let MStmt::Assign { value, .. } = stmt;
            // Special-case the content-invoke fallback emitted by
            // mir-lower for null-stubbed Compose wrapper calls — it uses
            // `CallKind::Virtual` to call `Function2.invoke(content,
            // null_composer, 0_changed)` with placeholders that we patch
            // here. Without this, the lambda body sees a null composer
            // and crashes on the first slot-table read.
            if let Rvalue::Call {
                kind:
                    CallKind::Virtual {
                        class_name,
                        method_name,
                    },
                args,
            } = value
            {
                if (class_name.starts_with("kotlin/jvm/functions/Function")
                    || class_name.contains("$Lambda$"))
                    && method_name == "invoke"
                    && args.len() >= 3
                {
                    // Patch args[1] (composer slot) and args[2] (changed slot).
                    if null_locals.contains(&args[1].0) {
                        args[1] = composer_local;
                    }
                    if zero_locals.contains(&args[2].0) {
                        args[2] = changed_local;
                    }
                    continue;
                }
            }
            let (descriptor, args, is_virtual): (&str, &mut Vec<LocalId>, bool) = match value {
                Rvalue::Call {
                    kind: CallKind::StaticJava { descriptor, .. },
                    args,
                } => (descriptor.as_str(), args, false),
                Rvalue::Call {
                    kind: CallKind::VirtualJava { descriptor, .. },
                    args,
                } => (descriptor.as_str(), args, true),
                _ => continue,
            };
            if !descriptor.contains("Composer;") {
                continue;
            }
            let composer_pos = match find_composer_pos_in_descriptor(descriptor) {
                Some(p) => p,
                None => continue,
            };
            // For VirtualJava, args[0] is the receiver — descriptor params
            // start at args[1]. Map descriptor-relative position to args
            // index.
            let arg_offset = if is_virtual { 1 } else { 0 };
            let composer_arg_idx = composer_pos + arg_offset;
            let changed_arg_idx = composer_arg_idx + 1;

            if let Some(slot) = args.get_mut(composer_arg_idx) {
                if *slot != composer_local && null_locals.contains(&slot.0) {
                    *slot = composer_local;
                }
            }
            if let Some(slot) = args.get_mut(changed_arg_idx) {
                if *slot != changed_local && zero_locals.contains(&slot.0) {
                    *slot = changed_local;
                }
            }
        }
    }
}

/// After the Compose transform adds $composer/$changed params to composable
/// functions, update all `CallKind::Static(fid)` call sites that target
/// those functions. These calls were lowered by MIR lowering BEFORE the
/// transform, so they have the original (pre-transform) arg count.
///
/// For each such call, append a null $composer placeholder and a zero
/// $changed placeholder to the args list.
fn patch_static_calls_to_composable(
    module: &mut MirModule,
    composable_param_types: &std::collections::HashMap<u32, Vec<Ty>>,
) {
    // Patch calls in top-level functions.
    for func in &mut module.functions {
        patch_calls_in_function(func, composable_param_types);
    }
    // Patch calls in class methods (including lambda invoke methods).
    for class in &mut module.classes {
        for method in &mut class.methods {
            patch_calls_in_function(method, composable_param_types);
        }
    }
}

#[allow(clippy::collapsible_if)]
fn patch_calls_in_function(
    func: &mut MirFunction,
    composable_param_types: &std::collections::HashMap<u32, Vec<Ty>>,
) {
    use skotch_mir::MirConst;

    for block in &mut func.blocks {
        // Collect patches to apply (we can't borrow stmts mutably while iterating).
        let mut patches: Vec<(usize, Vec<LocalId>)> = Vec::new();
        for (si, stmt) in block.stmts.iter().enumerate() {
            let MStmt::Assign { value, .. } = stmt;
            let needs_patch = match value {
                // CallKind::Static to a composable function (user-defined).
                Rvalue::Call {
                    kind: CallKind::Static(fid),
                    args,
                } if composable_param_types.contains_key(&fid.0) => {
                    let target_params = composable_param_types[&fid.0].len();
                    // Already has the right arg count → leave alone.
                    args.len() < target_params
                }
                // StaticJava/VirtualJava where descriptor mentions Composer
                // and arg count is below the descriptor's expected count.
                // Lift the previous `+3` cap so callers like
                // `JetchatTheme { ... }` (1 arg vs 5-param descriptor: 2 user
                // defaults + content + $composer + $changed) get filled in.
                Rvalue::Call {
                    kind: CallKind::StaticJava { descriptor, .. },
                    args,
                } => {
                    let desc_params = skotch_classinfo::count_descriptor_params_pub(descriptor);
                    desc_params > args.len() && descriptor.contains("Composer;")
                }
                Rvalue::Call {
                    kind: CallKind::VirtualJava { descriptor, .. },
                    args,
                } => {
                    let desc_params = skotch_classinfo::count_descriptor_params_pub(descriptor);
                    // VirtualJava: args include receiver, desc doesn't
                    let call_params = if args.is_empty() { 0 } else { args.len() - 1 };
                    desc_params > call_params && descriptor.contains("Composer;")
                }
                _ => false,
            };
            if needs_patch {
                // Skip if already patched: last 2+ args are [Composer, Int(changed), ...]
                let already_patched = match value {
                    Rvalue::Call { args, .. } if args.len() >= 2 => {
                        let pen = func.locals.get(args[args.len() - 2].0 as usize);
                        let last = func.locals.get(args[args.len() - 1].0 as usize);
                        matches!(pen, Some(Ty::Class(c)) if c.contains("Composer"))
                            && matches!(last, Some(Ty::Int))
                    }
                    _ => false,
                };
                if !already_patched {
                    // Compute how many args are missing, where the target's
                    // param list says the missing slots live (target_tys), AND
                    // if we have a JVM descriptor available, parse it to find
                    // the exact position of the Composer parameter — this is
                    // critical because the positional heuristic below assumes
                    // a `(...defaults, $composer, $changed)` tail but real
                    // composables with a `$default` mask emit
                    // `(...defaults, $composer, $changed, $default)`, putting
                    // the Composer at a different relative position. Without
                    // this descriptor probe, calls to compose-runtime
                    // functions like `rememberDrawerState` (5-param shape
                    // with `$default`) get the Composer arg placed in the
                    // `$changed` slot, leaving `$composer` as a null
                    // placeholder and crashing the runtime at the first
                    // `sourceInformationMarkerStart` call.
                    let (missing, target_tys, composer_pos): (
                        usize,
                        Option<&Vec<Ty>>,
                        Option<usize>,
                    ) = match value {
                        Rvalue::Call {
                            kind:
                                CallKind::StaticJava {
                                    descriptor,
                                    class_name,
                                    method_name,
                                },
                            args,
                        } => {
                            let dp = skotch_classinfo::count_descriptor_params_pub(descriptor);
                            let missing = dp.saturating_sub(args.len());
                            let abs_pos = find_composer_pos_in_descriptor(descriptor);
                            let rel_pos = abs_pos
                                .and_then(|p| p.checked_sub(args.len()))
                                .filter(|&p| p < missing);
                            if method_name == "rememberDrawerState" {
                                eprintln!(
                                        "DEBUG StaticJava patch: {}.{} desc={} args.len={} dp={} missing={} abs_pos={:?} rel_pos={:?}",
                                        class_name, method_name, descriptor, args.len(), dp,
                                        missing, abs_pos, rel_pos
                                    );
                            }
                            (missing, None, rel_pos)
                        }
                        Rvalue::Call {
                            kind: CallKind::VirtualJava { descriptor, .. },
                            args,
                        } => {
                            let dp = skotch_classinfo::count_descriptor_params_pub(descriptor);
                            let cp = if args.is_empty() { 0 } else { args.len() - 1 };
                            let missing = dp.saturating_sub(cp);
                            let abs_pos = find_composer_pos_in_descriptor(descriptor);
                            let rel_pos = abs_pos
                                .and_then(|p| p.checked_sub(cp))
                                .filter(|&p| p < missing);
                            (missing, None, rel_pos)
                        }
                        Rvalue::Call {
                            kind: CallKind::Static(fid),
                            args,
                        } => {
                            let tys = composable_param_types.get(&fid.0);
                            let total = tys.map(|v| v.len()).unwrap_or(0);
                            (total.saturating_sub(args.len()), tys, None)
                        }
                        _ => (2, None, None),
                    };
                    if missing == 0 {
                        continue;
                    }
                    let provided_count = match value {
                        Rvalue::Call { args, .. } => args.len(),
                        _ => 0,
                    };
                    // Add exactly `missing` args. For composable functions
                    // with default params, the pattern is:
                    //   user_defaults... + $composer + $changed [+ $default_mask]
                    // When we know the target's full param types (Static
                    // calls into known composables), use the actual type at
                    // index `provided_count + i` so user-default placeholders
                    // get the right `Ty` (Object/Any/Class) rather than Int.
                    // Otherwise, fall back to positional heuristics.
                    //
                    // For the `$composer` slot specifically, REUSE the
                    // enclosing function's existing Composer parameter
                    // instead of creating a fresh null local. Otherwise
                    // every call from inside a `@Composable` lambda to
                    // another `@Composable` receives `null` as its
                    // composer — which throws NPE inside the Compose
                    // runtime (e.g. `Composer.sourceInformationMarkerStart`
                    // on null in `rememberDrawerState`). This is the
                    // single biggest reason composable-heavy apps like
                    // JetChat crash mid-composition.
                    let enclosing_composer: Option<LocalId> =
                        func.params.iter().copied().find(|p| {
                            matches!(
                                func.locals.get(p.0 as usize),
                                Some(Ty::Class(c)) if c == "androidx/compose/runtime/Composer"
                            )
                        });
                    let mut extra_ids: Vec<LocalId> = Vec::new();
                    for i in 0..missing {
                        let ty = if let Some(tys) = target_tys {
                            tys.get(provided_count + i).cloned().unwrap_or(Ty::Any)
                        } else if Some(i) == composer_pos {
                            // Descriptor said position i is the Composer
                            // — use that authoritative answer.
                            Ty::Class("androidx/compose/runtime/Composer".to_string())
                        } else if let Some(cp) = composer_pos {
                            // Descriptor-driven: slots before the Composer
                            // are user-default reference args (Modifier,
                            // Function1, etc. — emit null). Slot cp+1 is
                            // `$changed` Int. Slot cp+2 (if present) is
                            // the `$default` mask Int. Without this,
                            // user-default slots like the leading
                            // `Modifier` of `DividerItem()` get filled
                            // with `Int(0)` and the JVM verifier rejects
                            // `iconst_0; checkcast Modifier`.
                            if i < cp {
                                Ty::Any
                            } else if i == cp + 1 || i == cp + 2 {
                                Ty::Int
                            } else {
                                Ty::Any
                            }
                        } else {
                            // No composer position from descriptor — fall
                            // back to the simple "last is $changed, rest
                            // are object" shape.
                            let is_last = missing.checked_sub(1) == Some(i);
                            let is_second_last = missing.checked_sub(2) == Some(i);
                            if is_second_last {
                                Ty::Class("androidx/compose/runtime/Composer".to_string())
                            } else if is_last {
                                Ty::Int
                            } else {
                                Ty::Any
                            }
                        };
                        let is_composer_slot = matches!(
                            &ty,
                            Ty::Class(c) if c == "androidx/compose/runtime/Composer"
                        );
                        let id = if is_composer_slot {
                            if let Some(existing) = enclosing_composer {
                                existing
                            } else {
                                let id = LocalId(func.locals.len() as u32);
                                func.locals.push(ty);
                                id
                            }
                        } else {
                            let id = LocalId(func.locals.len() as u32);
                            func.locals.push(ty);
                            id
                        };
                        extra_ids.push(id);
                    }
                    patches.push((si, extra_ids));
                }
            }
        }
        // Apply patches in reverse order to maintain statement indices.
        // Cache the enclosing Composer param so the per-call loop can
        // distinguish "freshly-allocated placeholder local" (needs a
        // null/0 initializer) from "reused enclosing-function $composer
        // param" (must NOT be reassigned — overwriting the param with
        // null is exactly the bug that made `rememberDrawerState` and
        // every other inner @Composable call receive a null Composer
        // at runtime).
        let enclosing_composer_for_apply: Option<LocalId> = func.params.iter().copied().find(|p| {
            matches!(
                func.locals.get(p.0 as usize),
                Some(Ty::Class(c)) if c == "androidx/compose/runtime/Composer"
            )
        });
        for (si, extra_ids) in patches.into_iter().rev() {
            // Insert assignment statements for all extra args before the call.
            let mut insert_count = 0;
            for (i, &id) in extra_ids.iter().enumerate() {
                // Skip emitting an init for the reused enclosing Composer
                // param — overwriting it with null defeats the entire
                // fix above.
                if Some(id) == enclosing_composer_for_apply {
                    continue;
                }
                let is_composer = i + 2 == extra_ids.len() && extra_ids.len() >= 2;
                let val = if is_composer {
                    Rvalue::Const(MirConst::Null) // $composer = null
                } else {
                    let ty = &func.locals[id.0 as usize];
                    if matches!(ty, Ty::Any | Ty::Class(_)) {
                        Rvalue::Const(MirConst::Null)
                    } else {
                        Rvalue::Const(MirConst::Int(0))
                    }
                };
                block.stmts.insert(
                    si + insert_count,
                    MStmt::Assign {
                        dest: id,
                        value: val,
                    },
                );
                insert_count += 1;
            }
            // Append all extra locals to the call's args.
            let MStmt::Assign { value, .. } = &mut block.stmts[si + insert_count];
            if let Rvalue::Call { args, .. } = value {
                for &id in &extra_ids {
                    args.push(id);
                }
            }
        }
    }
}

/// Parse a JVM method descriptor and return the 0-based index of the
/// `Landroidx/compose/runtime/Composer;` parameter, if any. Returns
/// `None` if the descriptor doesn't contain a Composer param. Used by
/// the @Composable arg-patching to place the `$composer` arg at the
/// right position when target type info isn't available (StaticJava /
/// VirtualJava call kinds) — the simple "second-from-last" heuristic
/// places it wrong when a `$default` mask is also present (e.g. the
/// 5-param `rememberDrawerState(DrawerValue, Function1, Composer, I, I)`
/// pattern that's everywhere in Compose Material).
fn find_composer_pos_in_descriptor(desc: &str) -> Option<usize> {
    let inside = desc.strip_prefix('(').and_then(|s| s.split_once(')'))?.0;
    let mut idx = 0usize;
    let mut chars = inside.chars();
    while let Some(c) = chars.next() {
        match c {
            'L' => {
                let mut name = String::new();
                for cc in chars.by_ref() {
                    if cc == ';' {
                        break;
                    }
                    name.push(cc);
                }
                if name == "androidx/compose/runtime/Composer" {
                    return Some(idx);
                }
                idx += 1;
            }
            '[' => continue,
            'B' | 'C' | 'D' | 'F' | 'I' | 'J' | 'S' | 'Z' => idx += 1,
            _ => continue,
        }
    }
    None
}

/// Check if a function has the `@Composable` annotation.
fn is_composable(func: &MirFunction) -> bool {
    func.annotations
        .iter()
        .any(|a| a.descriptor.contains("Composable"))
}

/// Transform a single `@Composable` function.
///
/// Injects `$composer` and `$changed` parameters, wraps body in
/// `startRestartGroup` / `endRestartGroup` calls.
fn transform_composable_function(func: &mut MirFunction) {
    let group_key = group_key_for(&func.name);

    // 1. Inject $composer parameter (Composer type).
    let composer_local = LocalId(func.locals.len() as u32);
    func.locals
        .push(Ty::Class("androidx/compose/runtime/Composer".to_string()));
    func.params.push(composer_local);
    func.param_names.push("$composer".to_string());

    // 2. Inject $changed bitmask parameter.
    let changed_local = LocalId(func.locals.len() as u32);
    func.locals.push(Ty::Int);
    func.params.push(changed_local);
    func.param_names.push("$changed".to_string());

    // kotlinc skips the restart-group wrapper for `inline` composable
    // functions — they're inlined at every call site so wrapping with
    // start/endRestartGroup would be dead code at the call site after
    // inlining. The $composer / $changed params still get injected
    // above for ABI compatibility; only the body wrap is elided.
    if func.is_inline {
        return;
    }

    // For non-inline composables, the JVM backend (skotch-backend-jvm)
    // handles ALL of the start/skip-check/body/end/restart-scope
    // emission as a single specialized path, bypassing MIR for the
    // wrapper bytecode. We store the group key as an annotation
    // marker the backend reads. Skip optimization, end-restart-group
    // dance, and the synthetic restart lambda are all emitted at the
    // bytecode level — far simpler than expressing them in MIR
    // (which would need new BitAnd/BitOr binops + CFG nodes).
    use skotch_mir::AnnotationRetention;
    func.annotations.push(skotch_mir::MirAnnotation {
        descriptor: "Lskotch/compose/$ComposeKey;".to_string(),
        args: vec![skotch_mir::MirAnnotationArg {
            name: "value".to_string(),
            value: skotch_mir::MirAnnotationValue::Int(group_key),
        }],
        retention: AnnotationRetention::Source,
    });

    // Thread the new $composer/$changed locals through composable call
    // sites in this function's body. MIR lowering ran before these
    // params existed, so those calls have `null` / `0` placeholder
    // locals at the $composer / $changed positions in their arg lists.
    // Without this, the body emits aconst_null for the composer arg
    // and the call invokes the Compose runtime with no live composer,
    // crashing on the first slot-table read.
    thread_composer_args(func, composer_local, changed_local);
}

/// Check if a module contains any `@Composable` functions.
pub fn has_composables(module: &MirModule) -> bool {
    module.functions.iter().any(is_composable)
        || module
            .classes
            .iter()
            .any(|c| c.methods.iter().any(is_composable))
}

/// Transform `remember { expr }` calls to `composer.cache(invalid, { expr })`.
///
/// Scans all composable functions for `remember` calls and rewrites them
/// to use the Composer's slot table for memoized caching.
pub fn transform_remember_calls(module: &mut MirModule) {
    for func in &mut module.functions {
        if !is_composable(func) {
            continue;
        }
        let composer_param = func
            .param_names
            .iter()
            .position(|n| n == "$composer")
            .map(|i| func.params[i]);
        let composer = match composer_param {
            Some(c) => c,
            None => continue,
        };

        // Rewrite remember calls in each block.
        for block in &mut func.blocks {
            for stmt in &mut block.stmts {
                let MStmt::Assign { dest, value } = stmt;
                if let Rvalue::Call {
                    kind:
                        CallKind::StaticJava {
                            ref class_name,
                            ref method_name,
                            ..
                        },
                    ref args,
                } = value
                {
                    if method_name == "remember" || class_name.contains("ComposablesKt") {
                        // Rewrite: remember(lambda) → composer.cache(false, lambda)
                        if !args.is_empty() {
                            let lambda_arg = *args.last().unwrap();
                            let invalid_local = LocalId(func.locals.len() as u32);
                            func.locals.push(Ty::Bool);
                            // We can't insert a new stmt before the current one
                            // mid-iteration, so we rewrite the call in-place.
                            *value = Rvalue::Call {
                                kind: CallKind::VirtualJava {
                                    class_name: "androidx/compose/runtime/Composer".to_string(),
                                    method_name: "cache".to_string(),
                                    descriptor:
                                        "(ZLkotlin/jvm/functions/Function0;)Ljava/lang/Object;"
                                            .to_string(),
                                },
                                args: vec![composer, invalid_local, lambda_arg],
                            };
                            let _ = dest; // result type stays the same
                        }
                    }
                }
            }
        }
    }
}

/// Detect `mutableStateOf(value)` calls and annotate them for the
/// Compose runtime's state tracking system.
///
/// In the full Compose runtime, `mutableStateOf` creates a `MutableState<T>`
/// that participates in snapshot state tracking. Reads during composition
/// are recorded, and writes trigger recomposition.
///
/// For now, this is a recognition pass — it identifies mutableStateOf calls
/// and marks them with metadata so the backend can generate the correct
/// runtime calls.
pub fn detect_state_calls(module: &MirModule) -> Vec<StateCallSite> {
    let mut sites = Vec::new();
    for (func_idx, func) in module.functions.iter().enumerate() {
        for (block_idx, block) in func.blocks.iter().enumerate() {
            for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
                let MStmt::Assign { value, .. } = stmt;
                if let Rvalue::Call {
                    kind:
                        CallKind::StaticJava {
                            ref method_name, ..
                        },
                    ..
                } = value
                {
                    if method_name == "mutableStateOf" {
                        sites.push(StateCallSite {
                            func_id: FuncId(func_idx as u32),
                            block_idx,
                            stmt_idx,
                        });
                    }
                }
            }
        }
    }
    sites
}

/// Location of a `mutableStateOf` call in the MIR.
#[derive(Clone, Debug)]
pub struct StateCallSite {
    pub func_id: FuncId,
    pub block_idx: usize,
    pub stmt_idx: usize,
}

/// Infer stability of classes for Compose skip optimization.
///
/// A class is "stable" if:
/// - All properties are `val` (immutable)
/// - All property types are stable primitives, String, or other stable classes
/// - No custom `equals` override
///
/// Stable classes allow the Compose runtime to skip recomposition when
/// parameter values haven't changed.
pub fn infer_stability(module: &MirModule) -> Vec<StabilityInfo> {
    module
        .classes
        .iter()
        .map(|cls| {
            let all_val = cls.fields.iter().all(|f| {
                // Heuristic: fields with primitive or String types are stable.
                matches!(
                    f.ty,
                    Ty::Int
                        | Ty::Long
                        | Ty::Double
                        | Ty::Bool
                        | Ty::Float
                        | Ty::String
                        | Ty::Byte
                        | Ty::Short
                        | Ty::Char
                )
            });
            let has_custom_equals = cls.methods.iter().any(|m| m.name == "equals");
            StabilityInfo {
                class_name: cls.name.clone(),
                is_stable: all_val && !has_custom_equals,
            }
        })
        .collect()
}

/// Stability classification for a class.
#[derive(Clone, Debug)]
pub struct StabilityInfo {
    pub class_name: String,
    pub is_stable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_mir::*;

    fn make_composable_function() -> MirFunction {
        MirFunction {
            id: FuncId(0),
            name: "Greeting".to_string(),
            params: vec![LocalId(0)],
            locals: vec![Ty::String], // one param: name: String
            blocks: vec![BasicBlock {
                stmts: vec![],
                terminator: Terminator::Return,
            }],
            return_ty: Ty::Unit,
            required_params: 1,
            param_names: vec!["name".to_string()],
            param_defaults: Vec::new(),
            param_receiver_types: Vec::new(),
            is_abstract: false,
            vararg_index: None,
            exception_handlers: Vec::new(),
            is_suspend: false,
            is_inline: false,
            has_type_params: false,
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: vec![MirAnnotation {
                descriptor: "Landroidx/compose/runtime/Composable;".to_string(),
                args: vec![],
                retention: AnnotationRetention::Runtime,
            }],
            named_locals: Vec::new(),
            is_private: false,
            is_static: false,
            default_call_masks: Vec::new(),
            needs_leading_nop: false,
            local_generic_args: Default::default(),
        }
    }

    #[test]
    fn detects_composable_annotation() {
        let func = make_composable_function();
        assert!(is_composable(&func));
    }

    #[test]
    fn non_composable_not_detected() {
        let mut func = make_composable_function();
        func.annotations.clear();
        assert!(!is_composable(&func));
    }

    #[test]
    fn transform_injects_composer_param() {
        let mut func = make_composable_function();
        assert_eq!(func.params.len(), 1); // just "name"
        transform_composable_function(&mut func);
        // Should now have name + $composer + $changed = 3 params
        assert_eq!(func.params.len(), 3);
        assert_eq!(func.param_names.last().unwrap(), "$changed");
        assert_eq!(func.param_names[func.param_names.len() - 2], "$composer");
    }

    #[test]
    fn transform_marks_with_compose_key_annotation() {
        let mut func = make_composable_function();
        transform_composable_function(&mut func);
        // Non-inline composables get marked with a $ComposeKey
        // annotation carrying the deterministic group key — the JVM
        // backend reads this to emit the canonical Compose dispatcher.
        assert!(func
            .annotations
            .iter()
            .any(|a| a.descriptor == "Lskotch/compose/$ComposeKey;"));
    }

    #[test]
    fn module_level_transform() {
        let mut module = MirModule::default();
        module.functions.push(make_composable_function());
        assert!(has_composables(&module));
        compose_transform(&mut module);
        // After transform, function should have 3 params
        assert_eq!(module.functions[0].params.len(), 3);
    }

    fn make_class(
        name: &str,
        fields: Vec<skotch_mir::MirField>,
        methods: Vec<MirFunction>,
    ) -> skotch_mir::MirClass {
        skotch_mir::MirClass {
            name: name.to_string(),
            super_class: None,
            is_open: false,
            is_abstract: false,
            is_interface: false,
            interfaces: vec![],
            fields,
            methods,
            constructor: make_composable_function(), // reuse as dummy
            secondary_constructors: vec![],
            is_suspend_lambda: false,
            is_cross_file_stub: false,
            annotations: vec![],
            has_type_params: false,
            is_object_singleton: false,
            companion_class_name: None,
            static_fields: Vec::new(),
            clinit: None,
        }
    }

    #[test]
    fn stability_inference_primitive_fields() {
        let mut module = MirModule::default();
        module.classes.push(make_class(
            "Point",
            vec![
                skotch_mir::MirField {
                    name: "x".to_string(),
                    ty: Ty::Int,
                    is_jvm_field: false,
                },
                skotch_mir::MirField {
                    name: "y".to_string(),
                    ty: Ty::Int,
                    is_jvm_field: false,
                },
            ],
            vec![],
        ));
        let stability = infer_stability(&module);
        assert_eq!(stability.len(), 1);
        assert!(stability[0].is_stable);
    }

    #[test]
    fn stability_unstable_with_custom_equals() {
        let mut module = MirModule::default();
        let mut equals_fn = make_composable_function();
        equals_fn.name = "equals".to_string();
        equals_fn.annotations.clear();
        module.classes.push(make_class(
            "Complex",
            vec![skotch_mir::MirField {
                name: "value".to_string(),
                ty: Ty::Int,
                is_jvm_field: false,
            }],
            vec![equals_fn],
        ));
        let stability = infer_stability(&module);
        assert!(!stability[0].is_stable);
    }
}
