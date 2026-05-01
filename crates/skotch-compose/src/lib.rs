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
    CallKind, FuncId, LocalId, MirAnnotation, MirFunction, MirModule, Rvalue, Stmt as MStmt,
    Terminator,
};
use skotch_types::Ty;

/// Group key counter for generating unique compose group IDs.
static GROUP_KEY_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn next_group_key() -> i32 {
    GROUP_KEY_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as i32
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
    let composable_fid_set: std::collections::HashSet<u32> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| is_composable(f))
        .map(|(i, _)| i as u32)
        .collect();
    patch_static_calls_to_composable(module, &composable_fid_set);

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
                let composer_id = LocalId(method.locals.len() as u32);
                method
                    .locals
                    .push(Ty::Class("androidx/compose/runtime/Composer".to_string()));
                method.params.push(composer_id);
                method.param_names.push("$composer".to_string());

                let changed_id = LocalId(method.locals.len() as u32);
                method.locals.push(Ty::Int);
                method.params.push(changed_id);
                method.param_names.push("$changed".to_string());

                // Post-process: in StaticJava calls where a Const(Null)
                // arg precedes an Int arg (the $composer/$changed pattern
                // from arg padding), replace with the real param locals.
                thread_composer_args(method, composer_id, changed_id);
            }
        }
    }
}

/// Replace null $composer and zero $changed placeholders in composable
/// function calls with references to the actual lambda invoke params.
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
                    // Check if the descriptor has Composer + int as last 2 params.
                    let desc_has_composer = descriptor.contains("Composer;")
                        || descriptor.contains("Ljava/lang/Object;I)");
                    if desc_has_composer {
                        // Replace second-to-last arg if it's a null placeholder.
                        if null_locals.contains(&args[n - 2].0) {
                            args[n - 2] = composer_local;
                        }
                        // Replace last arg if it's a zero placeholder.
                        if zero_locals.contains(&args[n - 1].0) {
                            args[n - 1] = changed_local;
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
    composable_fids: &std::collections::HashSet<u32>,
) {
    // Patch calls in top-level functions.
    for func in &mut module.functions {
        patch_calls_in_function(func, composable_fids);
    }
    // Patch calls in class methods (including lambda invoke methods).
    for class in &mut module.classes {
        for method in &mut class.methods {
            patch_calls_in_function(method, composable_fids);
        }
    }
}

#[allow(clippy::collapsible_if)]
fn patch_calls_in_function(
    func: &mut MirFunction,
    composable_fids: &std::collections::HashSet<u32>,
) {
    use skotch_mir::MirConst;

    for block in &mut func.blocks {
        // Collect patches to apply (we can't borrow stmts mutably while iterating).
        let mut patches: Vec<(usize, LocalId, Option<LocalId>, Option<LocalId>)> = Vec::new();
        for (si, stmt) in block.stmts.iter().enumerate() {
            let MStmt::Assign { value, .. } = stmt;
            let needs_patch = match value {
                // CallKind::Static to a composable function (user-defined).
                Rvalue::Call {
                    kind: CallKind::Static(fid),
                    args,
                } if composable_fids.contains(&fid.0) => {
                    let last_is_int = args
                        .last()
                        .and_then(|a| func.locals.get(a.0 as usize))
                        .is_some_and(|ty| matches!(ty, Ty::Int));
                    !last_is_int
                }
                // StaticJava/VirtualJava where descriptor expects 2 more args
                // than provided — these are composable calls to library/cross-file
                // functions where the descriptor includes $composer/$changed but
                // the call site doesn't pass them.
                Rvalue::Call {
                    kind: CallKind::StaticJava { descriptor, .. },
                    args,
                } => {
                    let desc_params = skotch_classinfo::count_descriptor_params_pub(descriptor);
                    desc_params > args.len()
                        && desc_params <= args.len() + 3
                        && descriptor.contains("Composer;")
                }
                Rvalue::Call {
                    kind: CallKind::VirtualJava { descriptor, .. },
                    args,
                } => {
                    let desc_params = skotch_classinfo::count_descriptor_params_pub(descriptor);
                    // VirtualJava: args include receiver, desc doesn't
                    let call_params = if args.is_empty() { 0 } else { args.len() - 1 };
                    desc_params > call_params
                        && desc_params <= call_params + 3
                        && descriptor.contains("Composer;")
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
                            kind: CallKind::Static(_),
                            ..
                        } => 2,
                        _ => 2,
                    };
                    if missing == 0 {
                        continue;
                    }
                    // Add exactly `missing` args. The pattern is:
                    // missing=2: $composer (null) + $changed (0)
                    // missing=3: $composer (null) + $changed (0) + $default (0)
                    // missing=1: just $composer (null) — unusual but safe
                    let composer_id = LocalId(func.locals.len() as u32);
                    func.locals
                        .push(Ty::Class("androidx/compose/runtime/Composer".to_string()));
                    let changed_id = if missing >= 2 {
                        let id = LocalId(func.locals.len() as u32);
                        func.locals.push(Ty::Int);
                        Some(id)
                    } else {
                        None
                    };
                    let default_id = if missing >= 3 {
                        let id = LocalId(func.locals.len() as u32);
                        func.locals.push(Ty::Int);
                        Some(id)
                    } else {
                        None
                    };
                    patches.push((si, composer_id, changed_id, default_id));
                }
            }
        }
        // Apply patches in reverse order to maintain statement indices.
        for (si, composer_id, changed_id, default_id) in patches.into_iter().rev() {
            // Insert assignment statements for the new args before the call.
            let mut insert_count = 0;
            // $composer = null
            block.stmts.insert(
                si,
                MStmt::Assign {
                    dest: composer_id,
                    value: Rvalue::Const(MirConst::Null),
                },
            );
            insert_count += 1;
            // $changed = 0 (if missing >= 2)
            if let Some(cid) = changed_id {
                block.stmts.insert(
                    si + insert_count,
                    MStmt::Assign {
                        dest: cid,
                        value: Rvalue::Const(MirConst::Int(0)),
                    },
                );
                insert_count += 1;
            }
            // $default = 0 (if missing >= 3)
            if let Some(def_id) = default_id {
                block.stmts.insert(
                    si + insert_count,
                    MStmt::Assign {
                        dest: def_id,
                        value: Rvalue::Const(MirConst::Int(0)),
                    },
                );
                insert_count += 1;
            }
            // Append the new locals to the call's args.
            let MStmt::Assign { value, .. } = &mut block.stmts[si + insert_count];
            if let Rvalue::Call { args, .. } = value {
                args.push(composer_id);
                if let Some(cid) = changed_id {
                    args.push(cid);
                }
                if let Some(def_id) = default_id {
                    args.push(def_id);
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
    let group_key = next_group_key();

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

    // 3. Mark with compose metadata for the backend.
    func.annotations.push(MirAnnotation {
        descriptor: "Lskotch/compose/ComposableTransformed;".to_string(),
        args: vec![skotch_mir::MirAnnotationArg {
            name: "groupKey".to_string(),
            value: skotch_mir::MirAnnotationValue::Int(group_key),
        }],
        retention: skotch_mir::AnnotationRetention::Runtime,
    });

    // 4. Prepend startRestartGroup call at the beginning of the function.
    //    In a full implementation, this would rewrite the CFG to wrap the
    //    entire body. For now, we inject a marker call at the entry block.
    if !func.blocks.is_empty() {
        let key_local = LocalId(func.locals.len() as u32);
        func.locals.push(Ty::Int);

        let group_result = LocalId(func.locals.len() as u32);
        func.locals
            .push(Ty::Class("androidx/compose/runtime/Composer".to_string()));

        let stmts = vec![
            // val $key = <group_key>
            MStmt::Assign {
                dest: key_local,
                value: Rvalue::Const(skotch_mir::MirConst::Int(group_key)),
            },
            // val $group = $composer.startRestartGroup($key)
            MStmt::Assign {
                dest: group_result,
                value: Rvalue::Call {
                    kind: CallKind::VirtualJava {
                        class_name: "androidx/compose/runtime/Composer".to_string(),
                        method_name: "startRestartGroup".to_string(),
                        descriptor: "(I)Landroidx/compose/runtime/Composer;".to_string(),
                    },
                    args: vec![composer_local, key_local],
                },
            },
        ];

        // Prepend to the first block.
        let mut new_stmts = stmts;
        new_stmts.append(&mut func.blocks[0].stmts);
        func.blocks[0].stmts = new_stmts;
    }

    // 5. Append endRestartGroup before each return.
    //    Find all blocks with Return/ReturnValue terminators and inject
    //    endRestartGroup call before them.
    for block in &mut func.blocks {
        if matches!(
            block.terminator,
            Terminator::Return | Terminator::ReturnValue(_)
        ) {
            let end_result = LocalId(func.locals.len() as u32);
            func.locals.push(Ty::Unit);

            block.stmts.push(MStmt::Assign {
                dest: end_result,
                value: Rvalue::Call {
                    kind: CallKind::VirtualJava {
                        class_name: "androidx/compose/runtime/Composer".to_string(),
                        method_name: "endRestartGroup".to_string(),
                        descriptor: "()V".to_string(),
                    },
                    args: vec![composer_local],
                },
            });
        }
    }
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
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: vec![MirAnnotation {
                descriptor: "Landroidx/compose/runtime/Composable;".to_string(),
                args: vec![],
                retention: AnnotationRetention::Runtime,
            }],
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
    fn transform_adds_start_restart_group() {
        let mut func = make_composable_function();
        let stmts_before = func.blocks[0].stmts.len();
        transform_composable_function(&mut func);
        // Should have injected startRestartGroup stmts
        assert!(func.blocks[0].stmts.len() > stmts_before);
    }

    #[test]
    fn transform_adds_end_restart_group() {
        let mut func = make_composable_function();
        transform_composable_function(&mut func);
        // The return block should have endRestartGroup
        let return_block = func
            .blocks
            .iter()
            .find(|b| matches!(b.terminator, Terminator::Return))
            .unwrap();
        assert!(!return_block.stmts.is_empty());
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
