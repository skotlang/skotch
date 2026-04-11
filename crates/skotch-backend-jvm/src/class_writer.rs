//! JVM class file emitter.
//!
//! Walks a [`MirModule`] and produces a `(class_name, bytes)` pair for
//! the wrapper class. Top-level functions in `Hello.kt` end up as
//! `public static` methods on a synthetic `HelloKt` class — matching
//! `kotlinc`'s wrapper-class convention.
//!
//! ## Single-pass codegen
//!
//! For each MIR function we walk the basic block once and emit
//! bytecode straight into a buffer. We track stack depth and the high
//! watermark in lockstep so the resulting `Code` attribute can record
//! `max_stack` correctly. Locals are assigned JVM slots on first use.
//!
//! Branch-free methods do not need a `StackMapTable`; PR #1 fixtures
//! avoid branches, so we don't emit one.

use crate::constant_pool::ConstantPool;
use byteorder::{BigEndian, WriteBytesExt};
use rustc_hash::FxHashMap;
use skotch_config::jvm;
use skotch_intern::Interner;
use skotch_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, LocalId, MirConst, MirFunction, MirModule, Rvalue, Stmt,
    Terminator,
};
use skotch_types::Ty;
use std::io::Write;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_SUPER: u16 = 0x0020;

/// A branch target inside a single block's codegen (from comparison patterns).
/// The comparison `if_icmpXX +7 / iconst_0 / goto +4 / iconst_1` creates
/// two targets (L_true at +7 and L_end at +8) that need StackMapTable entries.
struct CmpBranchTarget {
    /// Byte offset in the code array.
    offset: usize,
    /// Number of stack items at this target (0 for L_true, 1 for L_end).
    stack_count: u16,
    /// Byte offset where the comparison pattern starts (the if_icmpXX insn).
    cmp_start: usize,
    /// Index of the block containing this comparison.
    block_idx: usize,
}

/// Compile a [`MirModule`] to one (or more) `(internal_name, bytes)`
/// pairs ready to write to disk.
pub fn compile_module(module: &MirModule, _interner: &Interner) -> Vec<(String, Vec<u8>)> {
    let bytes = compile_class(&module.wrapper_class, module);
    vec![(module.wrapper_class.clone(), bytes)]
}

fn compile_class(class_name: &str, module: &MirModule) -> Vec<u8> {
    let mut cp = ConstantPool::new();
    let this_class_idx = cp.class(class_name);
    let super_class_idx = cp.class("java/lang/Object");
    let code_attr_name_idx = cp.utf8("Code");
    let source_simple = class_name
        .strip_suffix("Kt")
        .map(|s| format!("{s}.kt"))
        .unwrap_or_else(|| format!("{class_name}.kt"));
    let source_file_attr_name_idx = cp.utf8("SourceFile");
    let source_file_value_idx = cp.utf8(&source_simple);

    // Compile each method body first; the constant pool grows as a
    // side effect, and the methods reference its indices.
    let mut method_blobs: Vec<Vec<u8>> = Vec::new();
    for func in &module.functions {
        let blob = emit_method(func, module, class_name, &mut cp, code_attr_name_idx);
        method_blobs.push(blob);
    }

    let mut out: Vec<u8> = Vec::with_capacity(1024);
    out.write_u32::<BigEndian>(jvm::CLASS_FILE_MAGIC).unwrap();
    out.write_u16::<BigEndian>(jvm::CLASS_FILE_MINOR).unwrap();
    out.write_u16::<BigEndian>(jvm::DEFAULT_CLASS_FILE_MAJOR)
        .unwrap();

    out.write_u16::<BigEndian>(cp.count()).unwrap();
    cp.write_to(&mut out);

    out.write_u16::<BigEndian>(ACC_PUBLIC | ACC_FINAL | ACC_SUPER)
        .unwrap();
    out.write_u16::<BigEndian>(this_class_idx).unwrap();
    out.write_u16::<BigEndian>(super_class_idx).unwrap();

    out.write_u16::<BigEndian>(0).unwrap(); // interfaces_count
    out.write_u16::<BigEndian>(0).unwrap(); // fields_count

    out.write_u16::<BigEndian>(method_blobs.len() as u16)
        .unwrap();
    for blob in &method_blobs {
        out.extend_from_slice(blob);
    }

    out.write_u16::<BigEndian>(1).unwrap(); // attributes_count
    out.write_u16::<BigEndian>(source_file_attr_name_idx)
        .unwrap();
    out.write_u32::<BigEndian>(2).unwrap();
    out.write_u16::<BigEndian>(source_file_value_idx).unwrap();

    out
}

fn emit_method(
    func: &MirFunction,
    module: &MirModule,
    class_name: &str,
    cp: &mut ConstantPool,
    code_attr_name_idx: u16,
) -> Vec<u8> {
    let descriptor = jvm_descriptor(func);
    let access_flags = ACC_PUBLIC | ACC_STATIC;
    let name_idx = cp.utf8(&func.name);
    let descriptor_idx = cp.utf8(&descriptor);

    let mut code = Vec::<u8>::new();
    let mut stack: i32 = 0;
    let mut max_stack: i32 = 0;
    let mut slots: FxHashMap<u32, u8> = FxHashMap::default();
    let mut next_slot: u8 = 0;

    if func.name == "main" {
        next_slot = 1;
    } else {
        for &p in &func.params {
            slots.insert(p.0, next_slot);
            next_slot += 1;
        }
    }

    // ── Two-pass multi-block codegen ─────────────────────────────────
    //
    // Pass 1: emit each block's statements + terminator into `code`.
    //   Record the byte offset of each block's start and the positions
    //   of branch/goto instructions that need forward-patching.
    //
    // Pass 2: patch all branch/goto offsets to their target block's
    //   byte offset. JVM branch offsets are relative to the branch
    //   instruction itself.
    //
    // Then build a StackMapTable for every block that's the target of
    // a forward branch.
    struct JumpPatch {
        /// Byte offset in `code` of the u16 branch-offset field.
        offset_pos: usize,
        /// The byte offset of the branch instruction itself.
        insn_pos: usize,
        /// Index of the target block.
        target_block: u32,
    }

    let mut block_offsets: Vec<usize> = Vec::with_capacity(func.blocks.len());
    let mut patches: Vec<JumpPatch> = Vec::new();
    // Which blocks are branch/goto targets (need a StackMapTable entry).
    let mut is_target = vec![false; func.blocks.len()];
    // Comparison-internal branch targets (L_true / L_end from if_icmpXX patterns).
    let mut cmp_targets: Vec<CmpBranchTarget> = Vec::new();

    for (bi, block) in func.blocks.iter().enumerate() {
        block_offsets.push(code.len());

        walk_block(
            block,
            bi,
            cp,
            module,
            func,
            class_name,
            &mut code,
            &mut stack,
            &mut max_stack,
            &mut slots,
            &mut next_slot,
            &mut cmp_targets,
        );

        match &block.terminator {
            Terminator::Return => code.push(0xB1), // return (void)
            Terminator::ReturnValue(local) => {
                load_local(
                    &mut code,
                    &mut stack,
                    &mut max_stack,
                    &mut slots,
                    *local,
                    &func.locals,
                );
                let ty = &func.locals[local.0 as usize];
                match ty {
                    Ty::Int | Ty::Bool => code.push(0xAC), // ireturn
                    _ => code.push(0xB0),                  // areturn
                }
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // Load cond, branch to else if false, fall through to then.
                load_local(
                    code.as_mut(),
                    &mut stack,
                    &mut max_stack,
                    &mut slots,
                    *cond,
                    &func.locals,
                );
                // ifeq <else_offset> — jump to else block if cond == 0
                let insn_pos = code.len();
                code.push(0x99); // ifeq
                let offset_pos = code.len();
                code.write_i16::<BigEndian>(0).unwrap(); // placeholder
                bump(&mut stack, &mut max_stack, -1);
                patches.push(JumpPatch {
                    offset_pos,
                    insn_pos,
                    target_block: *else_block,
                });
                is_target[*else_block as usize] = true;
                // Fall through to then_block. If then_block isn't the
                // next sequential block, emit a goto.
                if *then_block as usize != bi + 1 {
                    let insn2 = code.len();
                    code.push(0xA7); // goto
                    let off2 = code.len();
                    code.write_i16::<BigEndian>(0).unwrap();
                    patches.push(JumpPatch {
                        offset_pos: off2,
                        insn_pos: insn2,
                        target_block: *then_block,
                    });
                    is_target[*then_block as usize] = true;
                }
            }
            Terminator::Goto(target) => {
                let insn_pos = code.len();
                code.push(0xA7); // goto
                let offset_pos = code.len();
                code.write_i16::<BigEndian>(0).unwrap();
                patches.push(JumpPatch {
                    offset_pos,
                    insn_pos,
                    target_block: *target,
                });
                is_target[*target as usize] = true;
            }
        }
    }

    // Pass 2: patch jump offsets.
    for patch in &patches {
        let target_off = block_offsets[patch.target_block as usize];
        let relative = (target_off as i32) - (patch.insn_pos as i32);
        let bytes = (relative as i16).to_be_bytes();
        code[patch.offset_pos] = bytes[0];
        code[patch.offset_pos + 1] = bytes[1];
    }

    let max_locals = next_slot as u16;

    // ── StackMapTable ────────────────────────────────────────────────
    //
    // Build entries for every branch/goto target: both inter-block
    // targets (from Terminator::Branch / Goto) and intra-block targets
    // (from comparison BinOp patterns: if_icmpXX / iconst_0 / goto /
    // iconst_1). Every target gets a `full_frame` (tag 255).
    //
    // The initial frame is: locals = [String[] for main, or params],
    // stack = [].
    let initial_locals_count: u16 = if func.name == "main" {
        1
    } else {
        func.params.len() as u16
    };
    let max_slots = next_slot as usize;

    // Build a global JVM-slot → MIR-local reverse map (for verification types).
    let mut slot_to_local: Vec<Option<u32>> = vec![None; max_slots];
    for (&mir_id, &jvm_slot) in slots.iter() {
        slot_to_local[jvm_slot as usize] = Some(mir_id);
    }

    // ── Per-block slot sets (fixed-point iteration for loops) ─────
    //
    // For acyclic CFGs a single forward pass suffices. Loops introduce
    // back-edges: the body block's live_at_end feeds into the condition
    // block's inherited set, but the body comes later in layout.
    // We iterate until live_at_end converges (typically 2–3 passes).
    // Initialize with all-true (optimistic, "top" in dataflow terms).
    // The fixed-point iteration narrows this to the correct set.
    let mut live_at_end: Vec<Vec<bool>> = vec![vec![true; max_slots]; func.blocks.len()];
    let mut inherited_per_block: Vec<Vec<bool>> = vec![vec![false; max_slots]; func.blocks.len()];
    for _iteration in 0..4 {
        let mut changed = false;
        for (bi, _) in func.blocks.iter().enumerate() {
            let mut inherited = vec![true; max_slots];
            let mut has_pred = false;
            for (pi, pblk) in func.blocks.iter().enumerate() {
                let is_pred = match &pblk.terminator {
                    Terminator::Branch {
                        then_block,
                        else_block,
                        ..
                    } => *then_block as usize == bi || *else_block as usize == bi,
                    Terminator::Goto(t) => *t as usize == bi,
                    _ => false,
                };
                if is_pred {
                    has_pred = true;
                    for s in 0..max_slots {
                        inherited[s] = inherited[s] && live_at_end[pi][s];
                    }
                }
            }
            if !has_pred && bi == 0 {
                for (s, val) in inherited.iter_mut().enumerate() {
                    *val = s < (initial_locals_count as usize);
                }
            } else if !has_pred {
                inherited = vec![false; max_slots];
            }
            inherited_per_block[bi] = inherited.clone();

            let start = block_offsets[bi];
            let end = if bi + 1 < block_offsets.len() {
                block_offsets[bi + 1]
            } else {
                code.len()
            };
            let mut assigned = inherited;
            scan_stores(&code[..end], start, end, max_slots, &mut assigned);
            if assigned != live_at_end[bi] {
                live_at_end[bi] = assigned;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // ── Collect ALL target offsets (inter-block + comparison-internal) ──
    // Each entry: (offset, is_cmp_target_index_or_none)
    enum TargetSource {
        Block(usize),
        Cmp(usize),
    }
    let mut all_targets: Vec<(usize, TargetSource)> = Vec::new();
    for (bi, &is_tgt) in is_target.iter().enumerate() {
        if is_tgt && bi < block_offsets.len() {
            all_targets.push((block_offsets[bi], TargetSource::Block(bi)));
        }
    }
    for (ci, ct) in cmp_targets.iter().enumerate() {
        all_targets.push((ct.offset, TargetSource::Cmp(ci)));
    }
    all_targets.sort_by_key(|&(off, _)| off);
    all_targets.dedup_by_key(|t| t.0);

    let mut stack_map_entries: Vec<u8> = Vec::new();
    let mut smt_count: u16 = 0;
    let mut prev_offset: i32 = -1;

    for &(off, ref source) in &all_targets {
        let delta = if prev_offset < 0 {
            off as i32
        } else {
            (off as i32) - prev_offset - 1
        };
        prev_offset = off as i32;
        smt_count += 1;

        match source {
            TargetSource::Block(target_bi) => {
                // Compute merged slot set from predecessors.
                let mut merged = vec![true; max_slots];
                let mut any_pred = false;
                for (pi, pblk) in func.blocks.iter().enumerate() {
                    let is_pred = match &pblk.terminator {
                        Terminator::Branch {
                            then_block,
                            else_block,
                            ..
                        } => {
                            *then_block as usize == *target_bi || *else_block as usize == *target_bi
                        }
                        Terminator::Goto(t) => *t as usize == *target_bi,
                        _ => false,
                    };
                    if is_pred {
                        any_pred = true;
                        for s in 0..max_slots {
                            merged[s] = merged[s] && live_at_end[pi][s];
                        }
                    }
                }
                if !any_pred {
                    merged = vec![false; max_slots];
                }

                let num_locals = merged
                    .iter()
                    .rposition(|&live| live)
                    .map(|i| (i + 1) as u16)
                    .unwrap_or(initial_locals_count);

                stack_map_entries.push(255); // full_frame
                stack_map_entries
                    .write_u16::<BigEndian>(delta as u16)
                    .unwrap();
                stack_map_entries
                    .write_u16::<BigEndian>(num_locals)
                    .unwrap();
                for slot in 0..num_locals as usize {
                    write_slot_verif(
                        &mut stack_map_entries,
                        cp,
                        slot,
                        merged.get(slot).copied().unwrap_or(false),
                        &slot_to_local,
                        func,
                    );
                }
                stack_map_entries.write_u16::<BigEndian>(0).unwrap(); // empty stack
            }
            TargetSource::Cmp(ci) => {
                let ct = &cmp_targets[*ci];
                // Compute live slots at the comparison point: inherited
                // set for this block + stores from block start to cmp_start.
                let mut live = inherited_per_block[ct.block_idx].clone();
                let blk_start = block_offsets[ct.block_idx];
                scan_stores(&code, blk_start, ct.cmp_start, max_slots, &mut live);

                let num_locals = live
                    .iter()
                    .rposition(|&v| v)
                    .map(|i| (i + 1) as u16)
                    .unwrap_or(initial_locals_count);

                stack_map_entries.push(255); // full_frame
                stack_map_entries
                    .write_u16::<BigEndian>(delta as u16)
                    .unwrap();
                stack_map_entries
                    .write_u16::<BigEndian>(num_locals)
                    .unwrap();
                for slot in 0..num_locals as usize {
                    write_slot_verif(
                        &mut stack_map_entries,
                        cp,
                        slot,
                        live.get(slot).copied().unwrap_or(false),
                        &slot_to_local,
                        func,
                    );
                }
                stack_map_entries
                    .write_u16::<BigEndian>(ct.stack_count)
                    .unwrap();
                // Integer_variable_info
                stack_map_entries.extend(std::iter::repeat_n(1u8, ct.stack_count as usize));
            }
        }
    }

    // Build the Code attribute.
    let mut code_attr = Vec::<u8>::new();
    code_attr
        .write_u16::<BigEndian>(max_stack.max(0) as u16)
        .unwrap();
    code_attr.write_u16::<BigEndian>(max_locals.max(1)).unwrap();
    code_attr.write_u32::<BigEndian>(code.len() as u32).unwrap();
    code_attr.write_all(&code).unwrap();
    code_attr.write_u16::<BigEndian>(0).unwrap(); // exception_table_length

    // Sub-attributes: StackMapTable if we have branch targets.
    if smt_count > 0 {
        let smt_name_idx = cp.utf8("StackMapTable");
        code_attr.write_u16::<BigEndian>(1).unwrap(); // attributes_count = 1
        code_attr.write_u16::<BigEndian>(smt_name_idx).unwrap();
        let smt_len = 2 + stack_map_entries.len();
        code_attr.write_u32::<BigEndian>(smt_len as u32).unwrap();
        code_attr.write_u16::<BigEndian>(smt_count).unwrap();
        code_attr.write_all(&stack_map_entries).unwrap();
    } else {
        code_attr.write_u16::<BigEndian>(0).unwrap(); // attributes_count = 0
    }

    let mut method = Vec::<u8>::new();
    method.write_u16::<BigEndian>(access_flags).unwrap();
    method.write_u16::<BigEndian>(name_idx).unwrap();
    method.write_u16::<BigEndian>(descriptor_idx).unwrap();
    method.write_u16::<BigEndian>(1).unwrap(); // attributes_count = 1
    method.write_u16::<BigEndian>(code_attr_name_idx).unwrap();
    method
        .write_u32::<BigEndian>(code_attr.len() as u32)
        .unwrap();
    method.write_all(&code_attr).unwrap();
    method
}

fn jvm_descriptor(func: &MirFunction) -> String {
    if func.name == "main" {
        return "([Ljava/lang/String;)V".to_string();
    }
    let mut s = String::from("(");
    for &p in &func.params {
        let ty = &func.locals[p.0 as usize];
        s.push_str(jvm_type(ty));
    }
    s.push(')');
    s.push_str(jvm_type(&func.return_ty));
    s
}

fn jvm_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::Unit => "V",
        Ty::Bool => "Z",
        Ty::Int => "I",
        Ty::Long => "J",
        Ty::Double => "D",
        Ty::String => "Ljava/lang/String;",
        Ty::Any => "Ljava/lang/Object;",
        Ty::Nullable(inner) => jvm_type(inner),
        Ty::Error => "V",
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_block(
    block: &BasicBlock,
    block_idx: usize,
    cp: &mut ConstantPool,
    module: &MirModule,
    func: &MirFunction,
    class_name: &str,
    code: &mut Vec<u8>,
    stack: &mut i32,
    max_stack: &mut i32,
    slots: &mut FxHashMap<u32, u8>,
    next_slot: &mut u8,
    cmp_targets: &mut Vec<CmpBranchTarget>,
) {
    for stmt in &block.stmts {
        let Stmt::Assign { dest, value } = stmt;
        match value {
            Rvalue::Const(c) => {
                emit_load_const(cp, code, stack, max_stack, c, module);
                store_local(code, stack, slots, next_slot, *dest, &func.locals);
            }
            Rvalue::Local(src) => {
                load_local(code, stack, max_stack, slots, *src, &func.locals);
                store_local(code, stack, slots, next_slot, *dest, &func.locals);
            }
            Rvalue::BinOp { op, lhs, rhs } => {
                load_local(code, stack, max_stack, slots, *lhs, &func.locals);
                load_local(code, stack, max_stack, slots, *rhs, &func.locals);
                match op {
                    MBinOp::AddI | MBinOp::SubI | MBinOp::MulI | MBinOp::DivI | MBinOp::ModI => {
                        let opcode: u8 = match op {
                            MBinOp::AddI => 0x60,
                            MBinOp::SubI => 0x64,
                            MBinOp::MulI => 0x68,
                            MBinOp::DivI => 0x6C,
                            MBinOp::ModI => 0x70,
                            _ => unreachable!(),
                        };
                        code.push(opcode);
                        bump(stack, max_stack, -1); // two ints in, one int out
                    }
                    MBinOp::CmpEq
                    | MBinOp::CmpNe
                    | MBinOp::CmpLt
                    | MBinOp::CmpGt
                    | MBinOp::CmpLe
                    | MBinOp::CmpGe => {
                        // Check if we're comparing reference types (String).
                        let lhs_ty = &func.locals[lhs.0 as usize];
                        if matches!(lhs_ty, Ty::String)
                            && matches!(op, MBinOp::CmpEq | MBinOp::CmpNe)
                        {
                            // String equality: invokevirtual String.equals
                            //   → returns boolean (0/1)
                            // For CmpNe, invert the result.
                            let equals =
                                cp.methodref("java/lang/String", "equals", "(Ljava/lang/Object;)Z");
                            code.push(0xB6); // invokevirtual
                            code.write_u16::<BigEndian>(equals).unwrap();
                            bump(stack, max_stack, -1); // pops receiver + arg, pushes bool
                            if *op == MBinOp::CmpNe {
                                // Invert: 1 - result
                                code.push(0x04); // iconst_1
                                bump(stack, max_stack, 1);
                                code.push(0x5F); // swap
                                code.push(0x64); // isub
                                bump(stack, max_stack, -1);
                            }
                            store_local(code, stack, slots, next_slot, *dest, &func.locals);
                            continue;
                        }

                        // Integer comparison → push 0 or 1 (bool).
                        // JVM doesn't have a direct "compare and push
                        // bool" instruction; we use if_icmp + iconst.
                        //   if_icmp<op> L_true   (3 bytes)
                        //   iconst_0             (1 byte)
                        //   goto L_end           (3 bytes)
                        // L_true:                (offset = cmp_start + 7)
                        //   iconst_1             (1 byte)
                        // L_end:                 (offset = cmp_start + 8)
                        let branch_op: u8 = match op {
                            MBinOp::CmpEq => 0x9F, // if_icmpeq
                            MBinOp::CmpNe => 0xA0, // if_icmpne
                            MBinOp::CmpLt => 0xA1, // if_icmplt
                            MBinOp::CmpGe => 0xA2, // if_icmpge
                            MBinOp::CmpGt => 0xA3, // if_icmpgt
                            MBinOp::CmpLe => 0xA4, // if_icmple
                            _ => unreachable!(),
                        };
                        let cmp_start = code.len();
                        code.push(branch_op);
                        code.write_i16::<BigEndian>(7).unwrap(); // skip to L_true (3+1+3=7)
                        bump(stack, max_stack, -2); // pops both operands
                        code.push(0x03); // iconst_0 (false)
                        bump(stack, max_stack, 1);
                        code.push(0xA7); // goto L_end
                        code.write_i16::<BigEndian>(4).unwrap(); // skip 1+3=4
                                                                 // L_true:
                        code.push(0x04); // iconst_1 (true)
                                         // L_end: (stack has one int)

                        // Record intra-block branch targets for StackMapTable.
                        cmp_targets.push(CmpBranchTarget {
                            offset: cmp_start + 7, // L_true
                            stack_count: 0,
                            cmp_start,
                            block_idx,
                        });
                        cmp_targets.push(CmpBranchTarget {
                            offset: cmp_start + 8, // L_end
                            stack_count: 1,
                            cmp_start,
                            block_idx,
                        });
                    }
                }
                store_local(code, stack, slots, next_slot, *dest, &func.locals);
            }
            Rvalue::Call { kind, args } => match kind {
                CallKind::Println => {
                    let fr = cp.fieldref("java/lang/System", "out", "Ljava/io/PrintStream;");
                    code.push(0xB2); // getstatic
                    code.write_u16::<BigEndian>(fr).unwrap();
                    bump(stack, max_stack, 1);

                    if let Some(&a) = args.first() {
                        load_local(code, stack, max_stack, slots, a, &func.locals);
                        let arg_ty = &func.locals[a.0 as usize];
                        let descriptor = match arg_ty {
                            Ty::Bool => "(Z)V",
                            Ty::Int => "(I)V",
                            Ty::String => "(Ljava/lang/String;)V",
                            _ => "(Ljava/lang/Object;)V",
                        };
                        let mref = cp.methodref("java/io/PrintStream", "println", descriptor);
                        code.push(0xB6); // invokevirtual
                        code.write_u16::<BigEndian>(mref).unwrap();
                        bump(stack, max_stack, -2);
                    } else {
                        let mref = cp.methodref("java/io/PrintStream", "println", "()V");
                        code.push(0xB6);
                        code.write_u16::<BigEndian>(mref).unwrap();
                        bump(stack, max_stack, -1);
                    }
                    let _ = dest;
                }
                CallKind::Static(target_id) => {
                    for a in args {
                        load_local(code, stack, max_stack, slots, *a, &func.locals);
                    }
                    let target = &module.functions[target_id.0 as usize];
                    let descriptor = jvm_descriptor(target);
                    let mref = cp.methodref(class_name, &target.name, &descriptor);
                    code.push(0xB8); // invokestatic
                    code.write_u16::<BigEndian>(mref).unwrap();
                    if target.return_ty != Ty::Unit {
                        // Non-void: consumed args, pushed return value.
                        bump(stack, max_stack, -(args.len() as i32) + 1);
                    } else {
                        bump(stack, max_stack, -(args.len() as i32));
                    }
                    store_local(code, stack, slots, next_slot, *dest, &func.locals);
                }
                CallKind::PrintlnConcat => {
                    // Build a `StringBuilder`, append each part with
                    // a type-appropriate `append` overload, call
                    // `toString()`, then route the result to
                    // `PrintStream.println(String)`. The whole
                    // sequence stays branch-free, so we don't need
                    // a `StackMapTable`.
                    //
                    // Stack diagram (PS = PrintStream, SB = StringBuilder):
                    //
                    //     getstatic System.out      [PS]
                    //     new StringBuilder         [PS, SB]
                    //     dup                       [PS, SB, SB]
                    //     invokespecial <init>      [PS, SB]
                    //     <for each part:>
                    //         load part             [PS, SB, part]
                    //         invokevirtual append  [PS, SB]   (returns SB)
                    //     invokevirtual toString    [PS, String]
                    //     invokevirtual println     []
                    let fr = cp.fieldref("java/lang/System", "out", "Ljava/io/PrintStream;");
                    code.push(0xB2); // getstatic
                    code.write_u16::<BigEndian>(fr).unwrap();
                    bump(stack, max_stack, 1);

                    let sb_class = cp.class("java/lang/StringBuilder");
                    code.push(0xBB); // new
                    code.write_u16::<BigEndian>(sb_class).unwrap();
                    bump(stack, max_stack, 1);
                    code.push(0x59); // dup
                    bump(stack, max_stack, 1);
                    let init = cp.methodref("java/lang/StringBuilder", "<init>", "()V");
                    code.push(0xB7); // invokespecial
                    code.write_u16::<BigEndian>(init).unwrap();
                    bump(stack, max_stack, -1); // pops the duplicated SB

                    for &arg in args {
                        load_local(code, stack, max_stack, slots, arg, &func.locals);
                        let arg_ty = &func.locals[arg.0 as usize];
                        let append_desc = match arg_ty {
                            Ty::String => "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
                            Ty::Int => "(I)Ljava/lang/StringBuilder;",
                            Ty::Bool => "(Z)Ljava/lang/StringBuilder;",
                            Ty::Long => "(J)Ljava/lang/StringBuilder;",
                            Ty::Double => "(D)Ljava/lang/StringBuilder;",
                            _ => "(Ljava/lang/Object;)Ljava/lang/StringBuilder;",
                        };
                        let append = cp.methodref("java/lang/StringBuilder", "append", append_desc);
                        code.push(0xB6); // invokevirtual
                        code.write_u16::<BigEndian>(append).unwrap();
                        // append: pops [SB, arg], pushes [SB] → net -1
                        bump(stack, max_stack, -1);
                    }

                    let to_string = cp.methodref(
                        "java/lang/StringBuilder",
                        "toString",
                        "()Ljava/lang/String;",
                    );
                    code.push(0xB6); // invokevirtual
                    code.write_u16::<BigEndian>(to_string).unwrap();
                    // toString: pops [SB], pushes [String] → net 0
                    let _ = stack;

                    let println =
                        cp.methodref("java/io/PrintStream", "println", "(Ljava/lang/String;)V");
                    code.push(0xB6); // invokevirtual
                    code.write_u16::<BigEndian>(println).unwrap();
                    bump(stack, max_stack, -2); // pops [PS, String]
                    let _ = dest;
                }
            },
        }
    }
}

fn emit_load_const(
    cp: &mut ConstantPool,
    code: &mut Vec<u8>,
    stack: &mut i32,
    max_stack: &mut i32,
    c: &MirConst,
    module: &MirModule,
) {
    match c {
        MirConst::Unit => {}
        MirConst::Bool(b) => {
            code.push(if *b { 0x04 } else { 0x03 });
            bump(stack, max_stack, 1);
        }
        MirConst::Int(v) => emit_iconst(cp, code, stack, max_stack, *v),
        MirConst::String(sid) => {
            let s = module.lookup_string(*sid);
            let idx = cp.string(s);
            if idx <= u8::MAX as u16 {
                code.push(0x12); // ldc
                code.push(idx as u8);
            } else {
                code.push(0x13); // ldc_w
                code.write_u16::<BigEndian>(idx).unwrap();
            }
            bump(stack, max_stack, 1);
        }
    }
}

fn emit_iconst(
    cp: &mut ConstantPool,
    code: &mut Vec<u8>,
    stack: &mut i32,
    max_stack: &mut i32,
    v: i32,
) {
    match v {
        -1 => code.push(0x02),
        0 => code.push(0x03),
        1 => code.push(0x04),
        2 => code.push(0x05),
        3 => code.push(0x06),
        4 => code.push(0x07),
        5 => code.push(0x08),
        v if (-128..=127).contains(&v) => {
            code.push(0x10);
            code.push(v as u8);
        }
        v if (-32768..=32767).contains(&v) => {
            code.push(0x11);
            code.write_i16::<BigEndian>(v as i16).unwrap();
        }
        _ => {
            let idx = cp.integer(v);
            if idx <= u8::MAX as u16 {
                code.push(0x12);
                code.push(idx as u8);
            } else {
                code.push(0x13);
                code.write_u16::<BigEndian>(idx).unwrap();
            }
        }
    }
    bump(stack, max_stack, 1);
}

fn slot_for(slots: &mut FxHashMap<u32, u8>, next_slot: &mut u8, local: LocalId) -> u8 {
    if let Some(&s) = slots.get(&local.0) {
        return s;
    }
    let s = *next_slot;
    slots.insert(local.0, s);
    *next_slot += 1;
    s
}

fn store_local(
    code: &mut Vec<u8>,
    stack: &mut i32,
    slots: &mut FxHashMap<u32, u8>,
    next_slot: &mut u8,
    local: LocalId,
    locals: &[Ty],
) {
    let ty = &locals[local.0 as usize];
    if matches!(ty, Ty::Unit) {
        return;
    }
    let slot = slot_for(slots, next_slot, local);
    let opcode = match ty {
        Ty::Int | Ty::Bool => 0x36, // istore
        _ => 0x3A,                  // astore
    };
    code.push(opcode);
    code.push(slot);
    *stack -= 1;
}

fn load_local(
    code: &mut Vec<u8>,
    stack: &mut i32,
    max_stack: &mut i32,
    slots: &mut FxHashMap<u32, u8>,
    local: LocalId,
    locals: &[Ty],
) {
    let ty = &locals[local.0 as usize];
    if matches!(ty, Ty::Unit) {
        return;
    }
    let &slot = slots
        .get(&local.0)
        .expect("local must be stored before being loaded");
    let opcode = match ty {
        Ty::Int | Ty::Bool => 0x15, // iload
        _ => 0x19,                  // aload
    };
    code.push(opcode);
    code.push(slot);
    bump(stack, max_stack, 1);
}

fn bump(stack: &mut i32, max_stack: &mut i32, by: i32) {
    *stack += by;
    if *stack > *max_stack {
        *max_stack = *stack;
    }
    if *stack < 0 {
        *stack = 0;
    }
}

/// Scan bytecode from `start` to `end` for istore/astore instructions,
/// marking the corresponding slots as live in `assigned`.
fn scan_stores(code: &[u8], start: usize, end: usize, max_slots: usize, assigned: &mut [bool]) {
    let mut i = start;
    while i < end {
        let op = code[i];
        if (op == 0x36 || op == 0x3A) && i + 1 < end {
            let slot = code[i + 1] as usize;
            if slot < max_slots {
                assigned[slot] = true;
            }
            i += 2;
        } else {
            i += 1;
            #[allow(clippy::match_overlapping_arm)]
            match op {
                0x10 | 0x12 | 0x15 | 0x19 => i += 1,
                0x11 | 0x13 | 0x14 | 0x99 | 0x9A | 0xA7 | 0xB2 | 0xB6 | 0xB7 | 0xB8 | 0xBB => {
                    i += 2
                }
                0x9F..=0xA4 => i += 2,
                0x59 => {} // dup - 0 operand bytes
                _ => {}
            }
        }
    }
}

/// Write a StackMapTable verification_type_info for a single JVM slot.
fn write_slot_verif(
    out: &mut Vec<u8>,
    cp: &mut ConstantPool,
    slot: usize,
    live: bool,
    slot_to_local: &[Option<u32>],
    func: &MirFunction,
) {
    if slot == 0 && func.name == "main" {
        out.push(7); // Object_variable_info
        let idx = cp.class("[Ljava/lang/String;");
        out.write_u16::<BigEndian>(idx).unwrap();
    } else if live {
        if let Some(mir_id) = slot_to_local.get(slot).copied().flatten() {
            let ty = &func.locals[mir_id as usize];
            match ty {
                Ty::Int | Ty::Bool => out.push(1), // Integer_variable_info
                Ty::String => {
                    out.push(7); // Object_variable_info
                    let idx = cp.class("java/lang/String");
                    out.write_u16::<BigEndian>(idx).unwrap();
                }
                _ => out.push(1), // fallback to Integer
            }
        } else {
            out.push(1); // Integer fallback for unknown slot
        }
    } else {
        out.push(0); // Top_variable_info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_intern::Interner;
    use skotch_lexer::lex;
    use skotch_mir_lower::lower_file;
    use skotch_parser::parse_file;
    use skotch_resolve::resolve_file;
    use skotch_span::FileId;
    use skotch_typeck::type_check;

    fn compile(src: &str) -> (Vec<(String, Vec<u8>)>, skotch_diagnostics::Diagnostics) {
        let mut interner = Interner::new();
        let mut diags = skotch_diagnostics::Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        let ast = parse_file(&lf, &mut interner, &mut diags);
        let r = resolve_file(&ast, &mut interner, &mut diags);
        let t = type_check(&ast, &r, &mut interner, &mut diags);
        let m = lower_file(&ast, &r, &t, &mut interner, &mut diags, "HelloKt");
        let bytes = compile_module(&m, &interner);
        (bytes, diags)
    }

    fn class_bytes(src: &str) -> Vec<u8> {
        let (out, d) = compile(src);
        assert!(!d.has_errors(), "diagnostics: {:?}", d);
        assert_eq!(out.len(), 1);
        let (name, bytes) = &out[0];
        assert_eq!(name, "HelloKt");
        bytes.clone()
    }

    #[test]
    fn emit_empty_main_starts_with_magic_and_v61() {
        let bytes = class_bytes("fun main() {}");
        // CAFEBABE 0000 003D
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(&bytes[4..8], &[0x00, 0x00, 0x00, 0x3D]); // major 61
    }

    #[test]
    fn emit_hello_world_class_contains_hello_string() {
        let bytes = class_bytes(r#"fun main() { println("Hello, world!") }"#);
        assert!(
            bytes.windows(13).any(|w| w == b"Hello, world!"),
            "expected Hello, world! in constant pool"
        );
    }

    #[test]
    fn emit_println_int_uses_iv_descriptor() {
        let bytes = class_bytes("fun main() { println(42) }");
        // The descriptor "(I)V" must appear as a Utf8 entry.
        assert!(bytes.windows(4).any(|w| w == b"(I)V"));
    }

    #[test]
    fn emit_arithmetic() {
        let bytes = class_bytes("fun main() { println(1 + 2 * 3) }");
        // Sanity: still contains a class header and the println descriptor.
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert!(bytes.windows(4).any(|w| w == b"(I)V"));
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: emit_class_with_branches_has_stackmaptable
    // TODO: emit_class_with_long_constant_pool
    // TODO: emit_top_level_val_with_clinit
    // TODO: emit_class_with_string_template
}
