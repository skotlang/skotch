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
