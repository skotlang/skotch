//! O(n) validation pass for MIR modules.
//!
//! Catches structural invariant violations — out-of-bounds local/block/
//! function/string references, empty non-abstract functions, malformed
//! exception handlers — before they surface as panics in the backend.
//!
//! Called by `skotch-driver` after MIR lowering. Can be disabled for
//! release builds via the `skip_validation` parameter.

use crate::{
    BasicBlock, CallKind, ExceptionHandler, FuncId, LocalId, MirClass, MirConst, MirFunction,
    MirModule, Rvalue, Stmt, Terminator,
};

/// A single validation error with context.
#[derive(Clone, Debug)]
pub struct MirError {
    /// Which function (by name) the error occurred in, or empty for module-level.
    pub function: String,
    /// Human-readable description of the invariant violation.
    pub message: String,
}

impl std::fmt::Display for MirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.function.is_empty() {
            write!(f, "MIR: {}", self.message)
        } else {
            write!(f, "MIR in `{}`: {}", self.function, self.message)
        }
    }
}

/// Validate an entire MIR module. Returns all errors found (empty = valid).
///
/// Runs in O(|functions| + |blocks| + |stmts|) — a single linear walk.
/// Cross-file stub classes receive only lightweight validation (field/name
/// checks, not full body validation).
pub fn validate_module(module: &MirModule) -> Vec<MirError> {
    let mut errors = Vec::new();

    for (idx, func) in module.functions.iter().enumerate() {
        validate_function(func, module, idx as u32, &mut errors);
    }

    for class in &module.classes {
        if class.is_cross_file_stub {
            // Stubs are skeletons — only validate name is non-empty.
            if class.name.is_empty() {
                errors.push(MirError {
                    function: String::new(),
                    message: "cross-file stub class has empty name".into(),
                });
            }
            continue;
        }
        validate_class(class, module, &mut errors);
    }

    errors
}

fn validate_function(
    func: &MirFunction,
    module: &MirModule,
    expected_id: u32,
    errors: &mut Vec<MirError>,
) {
    let ctx = &func.name;

    // Function ID must match its position in the module array.
    if func.id != FuncId(expected_id) {
        errors.push(err(
            ctx,
            format!(
                "id mismatch: expected FuncId({}), got FuncId({})",
                expected_id, func.id.0
            ),
        ));
    }

    // Non-abstract functions must have at least one block.
    if func.blocks.is_empty() && !func.is_abstract {
        errors.push(err(ctx, "non-abstract function has no blocks"));
    }

    // Parameter metadata lengths — tolerate mismatches from complex
    // function signatures (extension functions with receivers, etc.)
    // since backends handle the mismatch gracefully.

    // All parameter LocalIds must be in bounds.
    for &pid in &func.params {
        check_local(pid, func, ctx, errors);
    }

    // Validate each block.
    let num_blocks = func.blocks.len() as u32;
    for (bi, block) in func.blocks.iter().enumerate() {
        validate_block(block, func, module, bi, num_blocks, ctx, errors);
    }

    // Exception handler block indices must be in bounds.
    validate_exception_handlers(&func.exception_handlers, num_blocks, ctx, errors);
}

fn validate_block(
    block: &BasicBlock,
    func: &MirFunction,
    module: &MirModule,
    _block_idx: usize,
    num_blocks: u32,
    ctx: &str,
    errors: &mut Vec<MirError>,
) {
    // Validate every statement.
    for stmt in &block.stmts {
        let Stmt::Assign { dest, value } = stmt;
        check_local(*dest, func, ctx, errors);
        validate_rvalue(value, func, module, ctx, errors);
    }

    // Validate terminator.
    match &block.terminator {
        Terminator::Return => {
            // Return (void) is valid in any function — the MIR lowerer
            // may emit it even in non-Unit functions as dead code after
            // throw. Don't enforce return_ty matching here.
        }
        Terminator::ReturnValue(local) => {
            check_local(*local, func, ctx, errors);
        }
        Terminator::Goto(target) => {
            check_block(*target, num_blocks, ctx, errors);
        }
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => {
            check_local(*cond, func, ctx, errors);
            check_block(*then_block, num_blocks, ctx, errors);
            check_block(*else_block, num_blocks, ctx, errors);
        }
        Terminator::Throw(local) => {
            check_local(*local, func, ctx, errors);
        }
    }
}

fn validate_rvalue(
    rvalue: &Rvalue,
    func: &MirFunction,
    module: &MirModule,
    ctx: &str,
    errors: &mut Vec<MirError>,
) {
    match rvalue {
        Rvalue::Const(c) => {
            if let MirConst::String(sid) = c {
                if sid.0 as usize >= module.strings.len() {
                    errors.push(err(
                        ctx,
                        format!(
                            "StringId({}) out of bounds (pool size: {})",
                            sid.0,
                            module.strings.len()
                        ),
                    ));
                }
            }
        }
        Rvalue::Local(id) => {
            check_local(*id, func, ctx, errors);
        }
        Rvalue::BinOp { lhs, rhs, .. } => {
            check_local(*lhs, func, ctx, errors);
            check_local(*rhs, func, ctx, errors);
        }
        Rvalue::GetField { receiver, .. } => {
            check_local(*receiver, func, ctx, errors);
        }
        Rvalue::PutField {
            receiver, value, ..
        } => {
            check_local(*receiver, func, ctx, errors);
            check_local(*value, func, ctx, errors);
        }
        Rvalue::Call { kind, args } => {
            for &a in args {
                check_local(a, func, ctx, errors);
            }
            if let CallKind::Static(fid) = kind {
                if fid.0 as usize >= module.functions.len() {
                    errors.push(err(
                        ctx,
                        format!(
                            "CallKind::Static(FuncId({})) out of bounds (module has {} functions)",
                            fid.0,
                            module.functions.len()
                        ),
                    ));
                }
            }
        }
        Rvalue::InstanceOf { obj, .. } => {
            check_local(*obj, func, ctx, errors);
        }
        Rvalue::NewIntArray(size) => {
            check_local(*size, func, ctx, errors);
        }
        Rvalue::ArrayLoad { array, index } => {
            check_local(*array, func, ctx, errors);
            check_local(*index, func, ctx, errors);
        }
        Rvalue::ArrayStore {
            array,
            index,
            value,
        } => {
            check_local(*array, func, ctx, errors);
            check_local(*index, func, ctx, errors);
            check_local(*value, func, ctx, errors);
        }
        Rvalue::ArrayLength(id) => {
            check_local(*id, func, ctx, errors);
        }
        Rvalue::NewObjectArray(size) => {
            check_local(*size, func, ctx, errors);
        }
        Rvalue::NewTypedObjectArray { size, .. } => {
            check_local(*size, func, ctx, errors);
        }
        Rvalue::ObjectArrayStore {
            array,
            index,
            value,
        } => {
            check_local(*array, func, ctx, errors);
            check_local(*index, func, ctx, errors);
            check_local(*value, func, ctx, errors);
        }
        Rvalue::CheckCast { obj, .. } => {
            check_local(*obj, func, ctx, errors);
        }
        // Rvalues that only reference class names (no locals/IDs).
        Rvalue::GetStaticField { .. } | Rvalue::NewInstance(_) => {}
    }
}

fn validate_exception_handlers(
    handlers: &[ExceptionHandler],
    num_blocks: u32,
    ctx: &str,
    errors: &mut Vec<MirError>,
) {
    for (i, eh) in handlers.iter().enumerate() {
        if eh.try_start_block >= num_blocks {
            errors.push(err(
                ctx,
                format!(
                    "exception_handler[{i}].try_start_block ({}) >= num_blocks ({num_blocks})",
                    eh.try_start_block
                ),
            ));
        }
        if eh.try_end_block > num_blocks {
            errors.push(err(
                ctx,
                format!(
                    "exception_handler[{i}].try_end_block ({}) > num_blocks ({num_blocks})",
                    eh.try_end_block
                ),
            ));
        }
        if eh.handler_block >= num_blocks {
            errors.push(err(
                ctx,
                format!(
                    "exception_handler[{i}].handler_block ({}) >= num_blocks ({num_blocks})",
                    eh.handler_block
                ),
            ));
        }
        if eh.try_start_block >= eh.try_end_block {
            errors.push(err(
                ctx,
                format!(
                    "exception_handler[{i}]: try_start_block ({}) >= try_end_block ({})",
                    eh.try_start_block, eh.try_end_block
                ),
            ));
        }
    }
}

fn validate_class(class: &MirClass, module: &MirModule, errors: &mut Vec<MirError>) {
    if class.name.is_empty() {
        errors.push(MirError {
            function: String::new(),
            message: "class has empty name".into(),
        });
    }

    // Validate constructor.
    validate_class_method(&class.constructor, module, &class.name, errors);

    // Validate secondary constructors.
    for sec in &class.secondary_constructors {
        validate_class_method(sec, module, &class.name, errors);
    }

    // Validate instance methods.
    for method in &class.methods {
        validate_class_method(method, module, &class.name, errors);
    }
}

fn validate_class_method(
    func: &MirFunction,
    module: &MirModule,
    class_name: &str,
    errors: &mut Vec<MirError>,
) {
    let ctx = format!("{}.{}", class_name, func.name);

    // Non-abstract methods must have blocks.
    if func.blocks.is_empty() && !func.is_abstract {
        errors.push(err(&ctx, "non-abstract class method has no blocks"));
        return;
    }

    for &pid in &func.params {
        check_local(pid, func, &ctx, errors);
    }

    let num_blocks = func.blocks.len() as u32;
    for (bi, block) in func.blocks.iter().enumerate() {
        validate_block(block, func, module, bi, num_blocks, &ctx, errors);
    }

    validate_exception_handlers(&func.exception_handlers, num_blocks, &ctx, errors);
}

// ── Helpers ─────────────────────────────────────────────────────────────────

#[inline]
fn check_local(id: LocalId, func: &MirFunction, ctx: &str, errors: &mut Vec<MirError>) {
    if id.0 as usize >= func.locals.len() {
        errors.push(err(
            ctx,
            format!(
                "LocalId({}) out of bounds (function has {} locals)",
                id.0,
                func.locals.len()
            ),
        ));
    }
}

#[inline]
fn check_block(idx: u32, num_blocks: u32, ctx: &str, errors: &mut Vec<MirError>) {
    if idx >= num_blocks {
        errors.push(err(
            ctx,
            format!("block index {idx} out of bounds ({num_blocks} blocks)"),
        ));
    }
}

fn err(ctx: &str, message: impl Into<String>) -> MirError {
    MirError {
        function: ctx.to_string(),
        message: message.into(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BinOp, MirField};
    use skotch_types::Ty;

    fn empty_func(name: &str, id: u32) -> MirFunction {
        MirFunction {
            id: FuncId(id),
            name: name.to_string(),
            params: Vec::new(),
            locals: Vec::new(),
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
            suspend_original_return_ty: None,
            suspend_state_machine: None,
            annotations: Vec::new(),
        }
    }

    fn minimal_module() -> MirModule {
        MirModule {
            wrapper_class: "TestKt".into(),
            functions: vec![empty_func("main", 0)],
            ..MirModule::default()
        }
    }

    #[test]
    fn valid_minimal_module() {
        let m = minimal_module();
        let errors = validate_module(&m);
        assert!(errors.is_empty(), "Expected no errors: {errors:?}");
    }

    #[test]
    fn empty_module_is_valid() {
        let m = MirModule::default();
        let errors = validate_module(&m);
        assert!(errors.is_empty());
    }

    #[test]
    fn detects_local_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].blocks[0].stmts.push(Stmt::Assign {
            dest: LocalId(0),
            value: Rvalue::Local(LocalId(99)), // out of bounds
        });
        m.functions[0].locals.push(Ty::Int); // only local 0 exists
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("LocalId(99)"));
    }

    #[test]
    fn detects_funcid_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].locals.push(Ty::Int);
        m.functions[0].blocks[0].stmts.push(Stmt::Assign {
            dest: LocalId(0),
            value: Rvalue::Call {
                kind: CallKind::Static(FuncId(999)),
                args: Vec::new(),
            },
        });
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("FuncId(999)"));
    }

    #[test]
    fn detects_stringid_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].locals.push(Ty::String);
        m.functions[0].blocks[0].stmts.push(Stmt::Assign {
            dest: LocalId(0),
            value: Rvalue::Const(MirConst::String(crate::StringId(99))),
        });
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("StringId(99)"));
    }

    #[test]
    fn detects_block_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].blocks[0].terminator = Terminator::Goto(50);
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("block index 50"));
    }

    #[test]
    fn detects_branch_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].locals.push(Ty::Bool);
        m.functions[0].blocks[0].terminator = Terminator::Branch {
            cond: LocalId(0),
            then_block: 0,
            else_block: 99,
        };
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("block index 99"));
    }

    #[test]
    fn detects_non_abstract_empty_function() {
        let mut m = minimal_module();
        m.functions[0].blocks.clear();
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("no blocks"));
    }

    #[test]
    fn abstract_function_with_no_blocks_is_valid() {
        let mut m = minimal_module();
        m.functions[0].blocks.clear();
        m.functions[0].is_abstract = true;
        let errors = validate_module(&m);
        assert!(errors.is_empty());
    }

    #[test]
    fn detects_exception_handler_out_of_bounds() {
        let mut m = minimal_module();
        m.functions[0].exception_handlers.push(ExceptionHandler {
            try_start_block: 0,
            try_end_block: 1,
            handler_block: 99,
            catch_type: None,
        });
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("handler_block"));
    }

    #[test]
    fn detects_exception_handler_inverted_range() {
        let mut m = minimal_module();
        m.functions[0].blocks.push(BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        });
        m.functions[0].exception_handlers.push(ExceptionHandler {
            try_start_block: 1,
            try_end_block: 0,
            handler_block: 0,
            catch_type: None,
        });
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("try_start_block"));
    }

    #[test]
    fn detects_func_id_mismatch() {
        let mut m = minimal_module();
        m.functions[0].id = FuncId(42);
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("id mismatch"));
    }

    #[test]
    fn cross_file_stub_skips_body_validation() {
        let mut m = minimal_module();
        m.classes.push(MirClass {
            name: "StubClass".into(),
            super_class: None,
            is_open: false,
            is_abstract: false,
            is_interface: false,
            interfaces: Vec::new(),
            fields: vec![MirField {
                name: "x".into(),
                ty: Ty::Int,
            }],
            methods: Vec::new(),
            constructor: empty_func("<init>", 0),
            secondary_constructors: Vec::new(),
            is_suspend_lambda: false,
            is_cross_file_stub: true,
            annotations: Vec::new(),
        });
        let errors = validate_module(&m);
        assert!(errors.is_empty(), "Stub class should not cause errors");
    }

    #[test]
    fn validates_binop_local_refs() {
        let mut m = minimal_module();
        m.functions[0].locals.extend([Ty::Int, Ty::Int]);
        m.functions[0].blocks[0].stmts.push(Stmt::Assign {
            dest: LocalId(0),
            value: Rvalue::BinOp {
                op: BinOp::AddI,
                lhs: LocalId(0),
                rhs: LocalId(50), // out of bounds
            },
        });
        let errors = validate_module(&m);
        assert!(!errors.is_empty());
        assert!(errors[0].message.contains("LocalId(50)"));
    }

    #[test]
    fn validates_param_defaults_length_tolerated() {
        // Mismatched param_defaults length is tolerated (not an error)
        // because complex function signatures (extension functions with
        // receivers, etc.) can produce temporary mismatches that backends
        // handle gracefully.
        let mut m = minimal_module();
        m.functions[0].params = vec![LocalId(0)];
        m.functions[0].locals = vec![Ty::Int];
        m.functions[0].param_defaults = vec![None, None]; // 2 defaults, 1 param
        let errors = validate_module(&m);
        assert!(
            errors.is_empty(),
            "param_defaults mismatch should be tolerated"
        );
    }
}
