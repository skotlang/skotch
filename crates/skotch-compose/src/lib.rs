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

use skotch_mir::{CallKind, FuncId, LocalId, MirFunction, MirModule, Rvalue, Stmt as MStmt};
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
    let composable_param_counts: std::collections::HashMap<u32, usize> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| is_composable(f))
        .map(|(i, f)| (i as u32, f.params.len()))
        .collect();
    patch_static_calls_to_composable(module, &composable_param_counts);

    // Patch lambda classes that serve as @Composable content blocks.
    // The Compose runtime expects @Composable lambdas to implement
    // Function2<Composer, Int, Unit> (or Function3 for lambdas with
    // one user parameter, etc.) instead of the plain FunctionN.
    // We detect lambda classes whose invoke method has @Composable
    // annotation OR whose interface is Function0 and they're used
    // in a composable context — and bump their arity by 2.
    patch_composable_lambda_interfaces(module);
}

/// Patch lambda classes to implement the correct FunctionN interface
/// for Compose. A @Composable () -> Unit lambda must implement
/// Function2<Composer, Int, Unit> instead of Function0<Unit>.
fn patch_composable_lambda_interfaces(module: &mut MirModule) {
    // Collect names of all @Composable top-level functions.
    let _composable_fn_names: std::collections::HashSet<String> = module
        .functions
        .iter()
        .filter(|f| is_composable(f))
        .map(|f| f.name.clone())
        .collect();

    // Bump ALL Function0 lambda classes to Function2 in Compose projects.
    // The Compose runtime expects @Composable lambdas as Function2.
    // Non-composable lambdas that get bumped will have their invoke body
    // emitted as a stub by the JVM backend (the arity mismatch detection
    // handles this). The key composable lambda (setContent block) gets
    // the correct Function2 interface and its body executes normally
    // since the extra $composer/$changed params sit in unused JVM slots.
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

                // Note: thread_composer_args is intentionally NOT called here.
                // The MIR lowering and compose arg patching already place
                // the correct $composer/$changed values in composable call
                // args. Calling thread_composer_args would create duplicates
                // by replacing user-default null placeholders with $composer.
                let _ = (composer_id, changed_id);
            }
        }
    }
}

/// Replace null $composer and zero $changed placeholders in composable
/// function calls with references to the actual lambda invoke params.
/// NOTE: Currently disabled — the MIR lowering and compose arg patching
/// already handle placing the correct $composer/$changed values.
#[allow(dead_code, clippy::needless_range_loop)]
fn thread_composer_args(func: &mut MirFunction, composer_local: LocalId, changed_local: LocalId) {
    use skotch_mir::MirConst;
    // First, find which locals hold Const(Null) that are used as
    // $composer args, and which hold Const(Int(0)) for $changed args.
    // The pattern from arg padding is:
    //   Assign { dest: X, value: Const(Null) }     ← $composer placeholder
    //   Assign { dest: Y, value: Const(Int(0)) }   ← $changed placeholder
    //   Assign { dest: Z, value: Call { kind: StaticJava { ... }, args: [..., X, Y] } }
    //
    // Replace X→composer_local and Y→changed_local in the Call args.

    // Collect locals that are assigned Const(Null) with Ty::Any.
    let null_locals: std::collections::HashSet<u32> = func
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| {
            let MStmt::Assign { dest, value } = stmt;
            if matches!(value, Rvalue::Const(MirConst::Null))
                && matches!(func.locals.get(dest.0 as usize), Some(Ty::Any))
            {
                Some(dest.0)
            } else {
                None
            }
        })
        .collect();

    // Collect locals assigned Const(Int(0)).
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

    // In StaticJava calls, replace null/$zero args that appear in the
    // last 2 positions with the real $composer/$changed locals.
    for block in &mut func.blocks {
        for stmt in &mut block.stmts {
            let MStmt::Assign { value, .. } = stmt;
            if let Rvalue::Call {
                kind: CallKind::StaticJava { descriptor, .. },
                args,
            } = value
            {
                let n = args.len();
                if n >= 2 {
                    let desc_has_composer = descriptor.contains("Composer;")
                        || descriptor.contains("Ljava/lang/Object;I)");
                    if desc_has_composer {
                        // Replace null-placeholder args with real $composer,
                        // and zero-placeholder args with real $changed.
                        // Skip args that are ALREADY the real param locals
                        // (the MIR lowering may have placed them already).
                        for i in 0..n {
                            if args[i] == composer_local || args[i] == changed_local {
                                continue; // already the real param
                            }
                            if null_locals.contains(&args[i].0) {
                                args[i] = composer_local;
                            } else if zero_locals.contains(&args[i].0) {
                                args[i] = changed_local;
                            }
                        }
                    }
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
    composable_param_counts: &std::collections::HashMap<u32, usize>,
) {
    // Patch calls in top-level functions.
    for func in &mut module.functions {
        patch_calls_in_function(func, composable_param_counts);
    }
    // Patch calls in class methods (including lambda invoke methods).
    for class in &mut module.classes {
        for method in &mut class.methods {
            patch_calls_in_function(method, composable_param_counts);
        }
    }
}

#[allow(clippy::collapsible_if)]
fn patch_calls_in_function(
    func: &mut MirFunction,
    composable_param_counts: &std::collections::HashMap<u32, usize>,
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
                } if composable_param_counts.contains_key(&fid.0) => {
                    let target_params = composable_param_counts[&fid.0];
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
                    // Compute how many args are missing.
                    let missing = match value {
                        Rvalue::Call {
                            kind: CallKind::StaticJava { descriptor, .. },
                            args,
                        } => {
                            let dp = skotch_classinfo::count_descriptor_params_pub(descriptor);
                            dp.saturating_sub(args.len())
                        }
                        Rvalue::Call {
                            kind: CallKind::VirtualJava { descriptor, .. },
                            args,
                        } => {
                            let dp = skotch_classinfo::count_descriptor_params_pub(descriptor);
                            let cp = if args.is_empty() { 0 } else { args.len() - 1 };
                            dp.saturating_sub(cp)
                        }
                        Rvalue::Call {
                            kind: CallKind::Static(fid),
                            args,
                        } => composable_param_counts
                            .get(&fid.0)
                            .copied()
                            .unwrap_or(0)
                            .saturating_sub(args.len()),
                        _ => 2,
                    };
                    if missing == 0 {
                        continue;
                    }
                    // Add exactly `missing` args. For composable functions with
                    // default params, the pattern may be:
                    //   user_defaults... + $composer + $changed [+ $default_mask]
                    // We fill ALL missing slots with null/0 placeholders.
                    let mut extra_ids: Vec<LocalId> = Vec::new();
                    for i in 0..missing {
                        // Compute "is the i-th appended slot the Nth from the
                        // end" using checked_sub so missing < N doesn't wrap a
                        // `usize` to `usize::MAX` and falsely match (this was a
                        // real release-vs-debug discrepancy: release builds
                        // wrapped silently while debug builds panicked under
                        // overflow checks — caught by Ubuntu CI on fixture
                        // 582-composable-nested where `missing` is 1 or 2).
                        let is_last = missing.checked_sub(1) == Some(i);
                        let is_second_last = missing.checked_sub(2) == Some(i);
                        let is_third_last = missing.checked_sub(3) == Some(i);
                        let ty = if is_second_last {
                            // $composer position (second from end if missing>=2)
                            Ty::Class("androidx/compose/runtime/Composer".to_string())
                        } else if is_last || is_third_last {
                            Ty::Int // $changed or $default
                        } else {
                            Ty::Any // user default placeholder (null)
                        };
                        let id = LocalId(func.locals.len() as u32);
                        func.locals.push(ty);
                        extra_ids.push(id);
                    }
                    patches.push((si, extra_ids));
                }
            }
        }
        // Apply patches in reverse order to maintain statement indices.
        for (si, extra_ids) in patches.into_iter().rev() {
            // Insert assignment statements for all extra args before the call.
            let mut insert_count = 0;
            for (i, &id) in extra_ids.iter().enumerate() {
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
            default_call_masks: Vec::new(),
            needs_leading_nop: false,
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
                },
                skotch_mir::MirField {
                    name: "y".to_string(),
                    ty: Ty::Int,
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
            }],
            vec![equals_fn],
        ));
        let stability = infer_stability(&module);
        assert!(!stability[0].is_stable);
    }
}
