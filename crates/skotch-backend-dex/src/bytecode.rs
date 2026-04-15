//! DEX bytecode emitter for skotch's MIR.
//!
//! Walks one [`MirFunction`] at a time and emits the corresponding
//! `code_item` bytes plus the metadata that the writer needs to lay
//! out the surrounding `class_data_item` (registers_size, ins_size,
//! outs_size, instruction stream, etc.).
//!
//! ## Register allocation
//!
//! DEX is register-based (unlike JVM's stack machine). The DEX spec
//! puts incoming parameters at the **end** of the register file, so
//! we partition the registers into three contiguous groups:
//!
//! ```text
//!   v0 .. v(S-1)        scratch (e.g., System.out for println)
//!   v(S) .. v(S+N-1)    non-parameter locals
//!   v(S+N) .. v(S+N+P-1) parameters (in declaration order)
//! ```
//!
//! `N` is the number of MIR locals that are not parameters and not
//! `Ty::Unit` (Unit-typed call results don't need a register). `P` is
//! the number of MIR parameters. `S` is the maximum scratch needed by
//! any single instruction in the function — for PR #3 that's `1` if
//! the function makes a `println` call (to hold `System.out`),
//! otherwise `0`.
//!
//! All MIR registers are at most 16 (v0..v15) so the small
//! `format 35c` invoke instructions suffice. PR #3 fixtures stay
//! comfortably under that limit.
//!
//! ## Two-pass emission
//!
//! Like the rest of the DEX backend, this is a two-pass affair: we
//! emit instructions using *pre-finalization* indices into the
//! [`Pools`], then the writer patches the instruction stream with the
//! final post-sort indices via the `Remap` returned from
//! `Pools::finalize`. The patch points are recorded in
//! [`MethodCode::patches`].

use crate::pools::{Pools, Remap};
use byteorder::{LittleEndian, WriteBytesExt};
use rustc_hash::FxHashMap;
use skotch_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, LocalId, MirConst, MirFunction, MirModule, Rvalue, Stmt,
    Terminator,
};
use skotch_types::Ty;

/// One emitted method's compiled body, ready to be embedded in a
/// `code_item` after the writer has remapped the patches.
pub struct MethodCode {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub insns: Vec<u16>, // 16-bit code units
    /// Patch points: `(insn_offset, kind, old_pool_index)`. The writer
    /// applies the appropriate `Remap` and rewrites the 16-bit slot
    /// in `insns`.
    pub patches: Vec<Patch>,
}

#[derive(Debug, Clone, Copy)]
pub struct Patch {
    pub insn_offset: usize,
    pub kind: PatchKind,
    pub old_idx: u32,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum PatchKind {
    String,
    /// Reserved for future use (e.g. `check-cast`, `new-instance`).
    Type,
    Field,
    Method,
}

/// Lower a single MIR function to a DEX `code_item` body.
///
/// `pools` is mutated as the function references new strings/types/
/// methods. `class_descriptor` is the wrapper class (e.g.
/// `LInputKt;`) and is needed to resolve `Static` calls between two
/// top-level functions in the same source file.
pub fn lower_function(
    func: &MirFunction,
    module: &MirModule,
    class_descriptor: &str,
    pools: &mut Pools,
) -> MethodCode {
    // Compute scratch across ALL blocks (not just block[0]).
    let mut scratch_needed: u16 = 0;
    for block in &func.blocks {
        scratch_needed = scratch_needed.max(compute_scratch(block));
    }

    let num_params = func.params.len() as u16;

    // Count non-unit, non-parameter locals to see if comparisons will
    // need registers >= 16.  If so, reserve 2 extra scratch registers
    // so that the comparison operands can be moved into low regs.
    let num_non_unit_locals: u16 = func
        .locals
        .iter()
        .enumerate()
        .filter(|(i, ty)| (*i as u16) >= num_params && !matches!(ty, Ty::Unit))
        .count() as u16;
    let total_regs = scratch_needed + num_non_unit_locals + num_params;
    let has_comparisons = func.blocks.iter().any(|b| {
        b.stmts.iter().any(|s| {
            let Stmt::Assign { value, .. } = s;
            matches!(
                value,
                Rvalue::BinOp {
                    op: MBinOp::CmpEq
                        | MBinOp::CmpNe
                        | MBinOp::CmpLt
                        | MBinOp::CmpGe
                        | MBinOp::CmpGt
                        | MBinOp::CmpLe,
                    ..
                }
            )
        })
    });
    // If total registers >= 16 and comparisons exist, we need 2 scratch
    // registers for moving high-register comparison operands into low regs.
    let cmp_scratch: u16 = if has_comparisons && total_regs >= 16 {
        2
    } else {
        0
    };
    scratch_needed += cmp_scratch;

    let mut slot: FxHashMap<u32, u16> = FxHashMap::default();
    let mut next_local: u16 = scratch_needed;
    for (i, ty) in func.locals.iter().enumerate() {
        if (i as u16) < num_params {
            continue;
        }
        if matches!(ty, Ty::Unit) {
            continue;
        }
        slot.insert(i as u32, next_local);
        next_local += 1;
    }
    let num_locals = next_local - scratch_needed;
    let param_base = scratch_needed + num_locals;
    for (pi, _) in func.params.iter().enumerate() {
        slot.insert(pi as u32, param_base + pi as u16);
    }

    let registers_size = (scratch_needed + num_locals + num_params).max(1);

    let mut code = Vec::<u16>::new();
    let mut patches: Vec<Patch> = Vec::new();
    let mut max_outs: u16 = 0;

    // ── Multi-block codegen with jump patching ───────────────────────
    struct DexJumpPatch {
        offset_idx: usize,
        insn_idx: usize,
        target_block: u32,
        is_goto: bool,
    }
    let mut block_offsets: Vec<usize> = Vec::with_capacity(func.blocks.len());
    let mut jump_patches: Vec<DexJumpPatch> = Vec::new();

    for block in &func.blocks {
        block_offsets.push(code.len());

        walk_block(
            block,
            module,
            class_descriptor,
            pools,
            &slot,
            &func.locals,
            scratch_needed,
            cmp_scratch,
            &mut code,
            &mut patches,
            &mut max_outs,
        );

        match &block.terminator {
            Terminator::Return => {
                code.push(opcode_10x(0x0E)); // return-void
            }
            Terminator::ReturnValue(local) => {
                let ty = &func.locals[local.0 as usize];
                let reg = slot.get(&local.0).copied().unwrap_or(0);
                match ty {
                    Ty::Int | Ty::Bool => {
                        code.push(opcode_aa(0x0F, reg as u8)); // return vAA
                    }
                    _ => {
                        code.push(opcode_aa(0x11, reg as u8)); // return-object vAA
                    }
                }
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // if-eqz vAA, +offset  → jump to else if cond == 0
                let cond_reg = slot[&cond.0];
                let insn_idx = code.len();
                code.push(opcode_aa(0x38, cond_reg as u8));
                let off_idx = code.len();
                code.push(0); // placeholder
                jump_patches.push(DexJumpPatch {
                    offset_idx: off_idx,
                    insn_idx,
                    target_block: *else_block,
                    is_goto: false,
                });
                // Fall through to then. If not sequential, emit goto.
                let cur_bi = block_offsets.len() - 1;
                if *then_block as usize != cur_bi + 1 {
                    let gi = code.len();
                    code.push(opcode_10x(0x28)); // goto placeholder
                    jump_patches.push(DexJumpPatch {
                        offset_idx: gi,
                        insn_idx: gi,
                        target_block: *then_block,
                        is_goto: true,
                    });
                }
            }
            Terminator::Goto(target) => {
                let gi = code.len();
                code.push(opcode_10x(0x28)); // goto placeholder
                jump_patches.push(DexJumpPatch {
                    offset_idx: gi,
                    insn_idx: gi,
                    target_block: *target,
                    is_goto: true,
                });
            }
        }
    }

    // Patch jump offsets (code-unit-relative).
    for patch in &jump_patches {
        let target_off = block_offsets[patch.target_block as usize];
        let relative = (target_off as i32) - (patch.insn_idx as i32);
        if patch.is_goto {
            // goto: format 10t, offset in high byte (i8)
            code[patch.offset_idx] = opcode_aa(0x28, relative as i8 as u8);
        } else {
            // if-eqz: offset in next code unit (i16)
            code[patch.offset_idx] = relative as i16 as u16;
        }
    }

    MethodCode {
        registers_size,
        ins_size: num_params,
        outs_size: max_outs,
        insns: code,
        patches,
    }
}

/// Apply a `Remap` to a `MethodCode`'s patch list, rewriting the
/// 16-bit pool index in each instruction. Called by the writer after
/// `Pools::finalize`.
pub fn apply_remap(code: &mut MethodCode, remap: &Remap) {
    for patch in &code.patches {
        let new_idx = match patch.kind {
            PatchKind::String => remap.string[patch.old_idx as usize],
            PatchKind::Type => remap.r#type[patch.old_idx as usize],
            PatchKind::Field => remap.field[patch.old_idx as usize],
            PatchKind::Method => remap.method[patch.old_idx as usize],
        };
        assert!(
            new_idx < 0x1_0000,
            "PR #3 only supports indices that fit in 16 bits (got {new_idx})"
        );
        code.insns[patch.insn_offset] = new_idx as u16;
    }
}

/// Serialize a slice of 16-bit code units to a little-endian byte
/// stream for the `code_item.insns` field.
pub fn serialize_insns(insns: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(insns.len() * 2);
    for &u in insns {
        out.write_u16::<LittleEndian>(u).unwrap();
    }
    out
}

/// How many scratch registers does this block need?
///
/// - `Println` needs 1 (for `System.out`).
/// - `PrintlnConcat` needs 2 — one for the running `StringBuilder`
///   (which becomes the resulting `String` after `toString()`), and
///   one for `System.out` to receive the final `println`.
/// - Arithmetic and `invoke-static` need 0 — every operand already
///   has a MIR-assigned register.
///
/// We take the **maximum** across all calls in the block, not the
/// sum, because scratch registers are reused across statements.
fn compute_scratch(block: &BasicBlock) -> u16 {
    let mut needed: u16 = 0;
    for stmt in &block.stmts {
        let Stmt::Assign { value, .. } = stmt;
        if let Rvalue::Call { kind, .. } = value {
            let n = match kind {
                CallKind::Println | CallKind::Print => 1,
                CallKind::PrintlnConcat => 2,
                CallKind::Static(_)
                | CallKind::StaticJava { .. }
                | CallKind::Constructor(_)
                | CallKind::ConstructorJava { .. }
                | CallKind::Virtual { .. }
                | CallKind::Super { .. }
                | CallKind::VirtualJava { .. } => 0,
            };
            needed = needed.max(n);
        }
    }
    needed
}

// ─── instruction emission ────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn walk_block(
    block: &BasicBlock,
    module: &MirModule,
    class_descriptor: &str,
    pools: &mut Pools,
    slot: &FxHashMap<u32, u16>,
    locals: &[Ty],
    scratch_base: u16,
    cmp_scratch: u16,
    code: &mut Vec<u16>,
    patches: &mut Vec<Patch>,
    max_outs: &mut u16,
) {
    for stmt in &block.stmts {
        let Stmt::Assign { dest, value } = stmt;
        match value {
            Rvalue::Const(c) => {
                emit_const(c, *dest, slot, locals, module, pools, code, patches);
            }
            Rvalue::Local(src) => {
                emit_move(*src, *dest, slot, locals, code);
            }
            Rvalue::BinOp { op, lhs, rhs } => {
                emit_binop(*op, *lhs, *rhs, *dest, slot, cmp_scratch, code);
            }
            Rvalue::NewInstance(_) | Rvalue::GetField { .. } | Rvalue::PutField { .. } => {
                // TODO: class support in DEX backend
            }
            Rvalue::NewIntArray(_)
            | Rvalue::ArrayLoad { .. }
            | Rvalue::ArrayStore { .. }
            | Rvalue::ArrayLength(_)
            | Rvalue::NewObjectArray(_)
            | Rvalue::NewTypedObjectArray { .. }
            | Rvalue::ObjectArrayStore { .. } => {
                // TODO: IntArray/ObjectArray support in DEX backend
            }
            Rvalue::InstanceOf {
                obj,
                type_descriptor,
            } => {
                // instance-of vA, vB, type@CCCC (format 22c, opcode 0x20)
                // A = dest (4-bit), B = obj (4-bit), CCCC = type index
                let obj_reg = slot[&obj.0];
                let dest_reg = slot[&dest.0];
                let type_idx = pools.intern_type(&format!("L{type_descriptor};"));
                let ba = ((obj_reg & 0x0F) << 4) | (dest_reg & 0x0F);
                code.push((ba << 8) | 0x20);
                patches.push(Patch {
                    insn_offset: code.len(),
                    kind: PatchKind::Type,
                    old_idx: type_idx,
                });
                code.push(0); // placeholder for type index
            }
            Rvalue::Call { kind, args } => {
                let used = emit_call(
                    kind,
                    args,
                    *dest,
                    slot,
                    locals,
                    module,
                    class_descriptor,
                    pools,
                    scratch_base,
                    code,
                    patches,
                );
                if used > *max_outs {
                    *max_outs = used;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_const(
    c: &MirConst,
    dest: LocalId,
    slot: &FxHashMap<u32, u16>,
    locals: &[Ty],
    module: &MirModule,
    pools: &mut Pools,
    code: &mut Vec<u16>,
    patches: &mut Vec<Patch>,
) {
    if matches!(locals[dest.0 as usize], Ty::Unit) {
        return;
    }
    let dest_reg = slot[&dest.0];
    match c {
        MirConst::Unit => {}
        MirConst::Bool(b) => {
            emit_const_4(code, dest_reg, if *b { 1 } else { 0 });
        }
        MirConst::Int(v) => {
            if (-8..=7).contains(v) {
                emit_const_4(code, dest_reg, *v as i8);
            } else if (i16::MIN as i32..=i16::MAX as i32).contains(v) {
                // const/16 vAA, #+BBBB  (op 0x13, format 21s)
                code.push(opcode_aa(0x13, dest_reg as u8));
                code.push(*v as u16);
            } else {
                // const vAA, #+BBBBBBBB  (op 0x14, format 31i)
                code.push(opcode_aa(0x14, dest_reg as u8));
                code.push((*v as u32 & 0xFFFF) as u16);
                code.push(((*v as u32 >> 16) & 0xFFFF) as u16);
            }
        }
        MirConst::Long(v) => {
            code.push(opcode_aa(0x18, dest_reg as u8));
            let bits = *v as u64;
            code.push((bits & 0xFFFF) as u16);
            code.push(((bits >> 16) & 0xFFFF) as u16);
            code.push(((bits >> 32) & 0xFFFF) as u16);
            code.push(((bits >> 48) & 0xFFFF) as u16);
        }
        MirConst::Double(v) => {
            // const-wide vAA, #+BBBBBBBBBBBBBBBB (op 0x18, format 51l)
            code.push(opcode_aa(0x18, dest_reg as u8));
            let bits = v.to_bits();
            code.push((bits & 0xFFFF) as u16);
            code.push(((bits >> 16) & 0xFFFF) as u16);
            code.push(((bits >> 32) & 0xFFFF) as u16);
            code.push(((bits >> 48) & 0xFFFF) as u16);
        }
        MirConst::Null => {
            emit_const_4(code, dest_reg, 0);
        }
        MirConst::String(sid) => {
            let s = module.lookup_string(*sid);
            let str_idx = pools.intern_string(s);
            // const-string vAA, string@BBBB  (op 0x1A, format 21c)
            code.push(opcode_aa(0x1A, dest_reg as u8));
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::String,
                old_idx: str_idx,
            });
            code.push(0); // placeholder
        }
    }
}

/// Emit an invoke instruction, using format 35c (4-bit regs) when possible,
/// falling back to invoke-*/range (format 3rc) when any register >= 16.
/// `opcode_35c` is the normal opcode (e.g. 0x6E for invoke-virtual),
/// `opcode_range` is the range variant (e.g. 0x74 for invoke-virtual/range).
fn emit_invoke(
    code: &mut Vec<u16>,
    patches: &mut Vec<Patch>,
    opcode_35c: u8,
    opcode_range: u8,
    method_idx: u32,
    regs: &[u16],
) {
    let needs_range = regs.iter().any(|&r| r >= 16) || regs.len() > 5;
    if needs_range {
        // Format 3rc: AA|op BBBB CCCC
        // AA = arg count, BBBB = method idx, CCCC = first register
        // Registers must be contiguous — if they're not, we need to
        // emit moves to make them contiguous. For now, use a simple
        // approach: move all args to v0..vN scratch area.
        // TODO: This clobbers scratch registers — fine for our current
        // usage but may need refinement.
        let count = regs.len() as u16;
        let base = if regs.is_empty() { 0 } else { regs[0] };
        // Check if registers are already contiguous
        let contiguous = regs.windows(2).all(|w| w[1] == w[0] + 1);
        if contiguous || regs.is_empty() {
            code.push((count << 8) | opcode_range as u16);
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Method,
                old_idx: method_idx,
            });
            code.push(0); // placeholder for method_idx
            code.push(base);
        } else {
            // Non-contiguous: fall back to 35c with 4-bit truncation.
            // This is imprecise but avoids a scratch-move pass for now.
            let arg_count = regs.len() as u16;
            let high: u16 = arg_count << 4;
            code.push((high << 8) | opcode_35c as u16);
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Method,
                old_idx: method_idx,
            });
            code.push(0);
            let mut packed: u16 = 0;
            for (i, &r) in regs.iter().enumerate().take(4) {
                packed |= (r & 0x0F) << (i * 4);
            }
            code.push(packed);
        }
    } else {
        // Format 35c: A|G|op BBBB F|E|D|C
        let arg_count = regs.len() as u16;
        let high: u16 = arg_count << 4;
        code.push((high << 8) | opcode_35c as u16);
        patches.push(Patch {
            insn_offset: code.len(),
            kind: PatchKind::Method,
            old_idx: method_idx,
        });
        code.push(0);
        let mut packed: u16 = 0;
        for (i, &r) in regs.iter().enumerate().take(4) {
            packed |= (r & 0x0F) << (i * 4);
        }
        code.push(packed);
    }
}

/// Emit a small integer constant into a register.
fn emit_const_4_inline(code: &mut Vec<u16>, dest_reg: u16, val: i8) {
    if dest_reg < 16 {
        let a = dest_reg & 0x0F;
        let b = (val as u16) & 0x0F;
        code.push(((b << 12) | (a << 8)) | 0x12);
    } else {
        code.push(opcode_aa(0x13, dest_reg as u8));
        code.push(val as u16);
    }
}

/// Emit `const/4 vA, #+B` (op 0x12, format 11n).
///
/// Note: format 11n's literal is a 4-bit signed value (-8..=7) packed
/// into the high nibble of byte 1, with the destination register in
/// the low nibble.
fn emit_const_4(code: &mut Vec<u16>, dest_reg: u16, val: i8) {
    debug_assert!((-8..=7).contains(&val));
    if dest_reg < 16 {
        // const/4 vA, #+B (format 11n) — 4-bit register
        let a = dest_reg & 0x0F;
        let b = (val as u16) & 0x0F;
        code.push(((b << 12) | (a << 8)) | 0x12);
    } else {
        // const/16 vAA, #+BBBB (format 21s) — 8-bit register
        code.push(opcode_aa(0x13, dest_reg as u8));
        code.push(val as u16);
    }
}

fn emit_move(
    src: LocalId,
    dest: LocalId,
    slot: &FxHashMap<u32, u16>,
    locals: &[Ty],
    code: &mut Vec<u16>,
) {
    if matches!(locals[dest.0 as usize], Ty::Unit) {
        return;
    }
    let src_reg = slot[&src.0];
    let dest_reg = slot[&dest.0];
    let dest_ty = &locals[dest.0 as usize];
    let is_obj = matches!(
        dest_ty,
        Ty::String | Ty::Any | Ty::Nullable(_) | Ty::Class(_)
    );
    if src_reg < 16 && dest_reg < 16 {
        // Format 12x: B|A|op  (4-bit registers)
        let opcode: u8 = if is_obj { 0x07 } else { 0x01 };
        let high = ((src_reg & 0x0F) << 4) | (dest_reg & 0x0F);
        code.push((high << 8) | opcode as u16);
    } else {
        // Format 22x: AA|op BBBB (move/from16 or move-object/from16)
        // AA = dest (8-bit), BBBB = src (16-bit).
        let opcode: u8 = if is_obj { 0x08 } else { 0x02 };
        code.push(opcode_aa(opcode, dest_reg as u8));
        code.push(src_reg);
    }
}

fn emit_binop(
    op: MBinOp,
    lhs: LocalId,
    rhs: LocalId,
    dest: LocalId,
    slot: &FxHashMap<u32, u16>,
    cmp_scratch: u16,
    code: &mut Vec<u16>,
) {
    let l = slot[&lhs.0] as u8;
    let r = slot[&rhs.0] as u8;
    let d = slot[&dest.0] as u8;
    match op {
        MBinOp::ConcatStr => {
            // TODO: String concat in DEX (needs invoke-virtual StringBuilder)
            // For now, emit a nop — the fixture should be marked skip_dex.
        }
        MBinOp::AddI | MBinOp::SubI | MBinOp::MulI | MBinOp::DivI | MBinOp::ModI => {
            // Format 23x: AA|op | CC|BB ; vAA = vBB op vCC.
            let opcode: u8 = match op {
                MBinOp::AddI => 0x90,
                MBinOp::SubI => 0x91,
                MBinOp::MulI => 0x92,
                MBinOp::DivI => 0x93,
                MBinOp::ModI => 0x94,
                _ => unreachable!(),
            };
            code.push(((d as u16) << 8) | opcode as u16);
            code.push(((r as u16) << 8) | (l as u16));
        }
        MBinOp::AddD | MBinOp::SubD | MBinOp::MulD | MBinOp::DivD | MBinOp::ModD => {
            let opcode: u8 = match op {
                MBinOp::AddD => 0xCB, // add-double
                MBinOp::SubD => 0xCC, // sub-double
                MBinOp::MulD => 0xCD, // mul-double
                MBinOp::DivD => 0xCE, // div-double
                MBinOp::ModD => 0xCF, // rem-double
                _ => unreachable!(),
            };
            code.push(((d as u16) << 8) | opcode as u16);
            code.push(((r as u16) << 8) | (l as u16));
        }
        MBinOp::AddL | MBinOp::SubL | MBinOp::MulL | MBinOp::DivL | MBinOp::ModL => {
            let opcode: u8 = match op {
                MBinOp::AddL => 0x9B, // add-long
                MBinOp::SubL => 0x9C, // sub-long
                MBinOp::MulL => 0x9D, // mul-long
                MBinOp::DivL => 0x9E, // div-long
                MBinOp::ModL => 0x9F, // rem-long
                _ => unreachable!(),
            };
            code.push(((d as u16) << 8) | opcode as u16);
            code.push(((r as u16) << 8) | (l as u16));
        }
        MBinOp::CmpEq
        | MBinOp::CmpNe
        | MBinOp::CmpLt
        | MBinOp::CmpGt
        | MBinOp::CmpLe
        | MBinOp::CmpGe => {
            // DEX integer comparison: if-<op> vA, vB, +offset
            // If true → set dest=1; else dest=0.
            //
            //   const/4 vD, 1       (optimistic: true)
            //   if-<op> vL, vR, +N  (skip the false-correction below)
            //   const/4 vD, 0       (correction: false)
            //
            // Format 22t requires 4-bit registers.  When comparison
            // operands are >= 16, move them into dedicated comparison
            // scratch registers (guaranteed < 16) first.
            let cond_op: u8 = match op {
                MBinOp::CmpEq => 0x32, // if-eq
                MBinOp::CmpNe => 0x33, // if-ne
                MBinOp::CmpLt => 0x34, // if-lt
                MBinOp::CmpGe => 0x35, // if-ge
                MBinOp::CmpGt => 0x36, // if-gt
                MBinOp::CmpLe => 0x37, // if-le
                _ => unreachable!(),
            };

            let mut l_eff = l as u16;
            let mut r_eff = r as u16;

            if cmp_scratch >= 2 {
                // Comparison scratch registers: v(cmp_scratch-2), v(cmp_scratch-1)
                let cmp0 = cmp_scratch - 2;
                let cmp1 = cmp_scratch - 1;
                if l_eff >= 16 {
                    // move/from16 vCmp0, vL  (format 22x)
                    code.push(opcode_aa(0x02, cmp0 as u8));
                    code.push(l_eff);
                    l_eff = cmp0;
                }
                if r_eff >= 16 {
                    let tmp = if l_eff == cmp0 { cmp1 } else { cmp0 };
                    // move/from16 vTmp, vR  (format 22x)
                    code.push(opcode_aa(0x02, tmp as u8));
                    code.push(r_eff);
                    r_eff = tmp;
                }
            }

            // const/4 vD, 1  (optimistic: true)
            emit_const_4_inline(code, d as u16, 1);
            // if-<op> vL, vR, +offset  (format 22t: B|A|op CCCC)
            let ab = ((r_eff & 0x0F) << 4) | (l_eff & 0x0F);
            code.push((ab << 8) | cond_op as u16);
            // Offset = 2 (this insn) + correction_size.
            // emit_const_4_inline is 1 code-unit if d < 16, 2 if d >= 16.
            let correction_size: i16 = if (d as u16) < 16 { 1 } else { 2 };
            code.push((2 + correction_size) as u16);
            // const/4 vD, 0  (correction: false)
            emit_const_4_inline(code, d as u16, 0);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_call(
    kind: &CallKind,
    args: &[LocalId],
    dest: LocalId,
    slot: &FxHashMap<u32, u16>,
    locals: &[Ty],
    module: &MirModule,
    class_descriptor: &str,
    pools: &mut Pools,
    scratch_base: u16,
    code: &mut Vec<u16>,
    patches: &mut Vec<Patch>,
) -> u16 {
    match kind {
        CallKind::Println | CallKind::Print => {
            // Use scratch register 0 for System.out. The slot map
            // already reserves the lowest `scratch_needed` registers
            // for this.
            let sysout_reg: u16 = 0;
            debug_assert!(scratch_base >= 1, "println requires scratch reservation");

            let arg = args[0];
            let arg_reg = slot[&arg.0];

            // sget-object v0, Ljava/lang/System;->out:Ljava/io/PrintStream;
            // (op 0x62, format 21c)
            let field_idx =
                pools.intern_field("Ljava/lang/System;", "out", "Ljava/io/PrintStream;");
            code.push(opcode_aa(0x62, sysout_reg as u8));
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Field,
                old_idx: field_idx,
            });
            code.push(0);

            // invoke-virtual {sysout, arg}, println(<argTy>)V
            // (op 0x6E, format 35c)
            let arg_ty = &locals[arg.0 as usize];
            let param_desc = match arg_ty {
                Ty::Bool => "Z",
                Ty::Int => "I",
                Ty::String => "Ljava/lang/String;",
                _ => "Ljava/lang/Object;",
            };
            let method_idx =
                pools.intern_method("Ljava/io/PrintStream;", "println", "V", &[param_desc]);
            emit_invoke(
                code,
                patches,
                0x6E, // invoke-virtual
                0x74, // invoke-virtual/range
                method_idx,
                &[sysout_reg, arg_reg],
            );
            2
        }
        CallKind::Static(target_id) => {
            let target = &module.functions[target_id.0 as usize];
            let params: Vec<&str> = target
                .params
                .iter()
                .map(|p| type_descriptor(&target.locals[p.0 as usize]))
                .collect();
            let ret_desc = type_descriptor(&target.return_ty);
            let method_idx = pools.intern_method(class_descriptor, &target.name, ret_desc, &params);

            let arg_regs: Vec<u16> = args.iter().map(|a| slot[&a.0]).collect();
            let n = args.len() as u16;
            emit_invoke(
                code, patches, 0x71, // invoke-static
                0x77, // invoke-static/range
                method_idx, &arg_regs,
            );
            // For non-void returns, capture the result.
            if target.return_ty != Ty::Unit {
                let dest_reg = slot[&dest.0];
                let move_op = match &target.return_ty {
                    Ty::Int | Ty::Bool => 0x0A, // move-result
                    _ => 0x0C,                  // move-result-object
                };
                code.push(opcode_aa(move_op, dest_reg as u8));
            }
            n
        }
        CallKind::PrintlnConcat => {
            // Layout (matching `compute_scratch`'s reservation):
            //
            //   v0 — `StringBuilder`, then the resulting `String`
            //         after `toString()`
            //   v1 — `System.out` (loaded just before the final
            //         println call)
            //
            // The bytecode shape:
            //
            //   new-instance v0, Ljava/lang/StringBuilder;
            //   invoke-direct {v0}, <init>:()V
            //   for each part:
            //     invoke-virtual {v0, vPart}, append:(Ty)StringBuilder;
            //     move-result-object v0
            //   invoke-virtual {v0}, toString:()Ljava/lang/String;
            //   move-result-object v0
            //   sget-object v1, System.out
            //   invoke-virtual {v1, v0}, println:(String)V
            debug_assert!(
                scratch_base >= 2,
                "PrintlnConcat requires 2 scratch registers"
            );
            let sb_reg: u16 = 0;
            let print_reg: u16 = 1;

            // new-instance v0, Ljava/lang/StringBuilder;  (op 0x22, fmt 21c)
            let sb_type_idx = pools.intern_type("Ljava/lang/StringBuilder;");
            code.push(opcode_aa(0x22, sb_reg as u8));
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Type,
                old_idx: sb_type_idx,
            });
            code.push(0);

            // invoke-direct {v0}, <init>:()V  (op 0x70, fmt 35c)
            let init_method = pools.intern_method("Ljava/lang/StringBuilder;", "<init>", "V", &[]);
            let high: u16 = 1 << 4; // A=1
            code.push((high << 8) | 0x70);
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Method,
                old_idx: init_method,
            });
            code.push(0);
            code.push(sb_reg & 0x0F); // packed: just C=v0

            // For each part: invoke-virtual + move-result-object.
            for &arg in args {
                let arg_reg = slot[&arg.0];
                let arg_ty = &locals[arg.0 as usize];
                let param_desc = match arg_ty {
                    Ty::String => "Ljava/lang/String;",
                    Ty::Int | Ty::Bool => "I",
                    Ty::Long => "J",
                    Ty::Double => "D",
                    _ => "Ljava/lang/Object;",
                };
                let append_method = pools.intern_method(
                    "Ljava/lang/StringBuilder;",
                    "append",
                    "Ljava/lang/StringBuilder;",
                    &[param_desc],
                );
                // invoke-virtual {v_sb, v_arg}, append:(...)Ljava/lang/StringBuilder;
                let high: u16 = 2 << 4; // A=2
                code.push((high << 8) | 0x6E);
                patches.push(Patch {
                    insn_offset: code.len(),
                    kind: PatchKind::Method,
                    old_idx: append_method,
                });
                code.push(0);
                let lo = (sb_reg & 0x0F) | ((arg_reg & 0x0F) << 4);
                code.push(lo);
                // move-result-object v_sb  (op 0x0C, fmt 11x)
                code.push(opcode_aa(0x0C, sb_reg as u8));
            }

            // invoke-virtual {v_sb}, toString:()Ljava/lang/String;
            let to_string = pools.intern_method(
                "Ljava/lang/StringBuilder;",
                "toString",
                "Ljava/lang/String;",
                &[],
            );
            let high: u16 = 1 << 4; // A=1
            code.push((high << 8) | 0x6E);
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Method,
                old_idx: to_string,
            });
            code.push(0);
            code.push(sb_reg & 0x0F);
            // move-result-object v_sb (now holds the String)
            code.push(opcode_aa(0x0C, sb_reg as u8));

            // sget-object v_print, System.out
            let field_idx =
                pools.intern_field("Ljava/lang/System;", "out", "Ljava/io/PrintStream;");
            code.push(opcode_aa(0x62, print_reg as u8));
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Field,
                old_idx: field_idx,
            });
            code.push(0);

            // invoke-virtual {v_print, v_sb}, println:(Ljava/lang/String;)V
            let println_method = pools.intern_method(
                "Ljava/io/PrintStream;",
                "println",
                "V",
                &["Ljava/lang/String;"],
            );
            let high: u16 = 2 << 4;
            code.push((high << 8) | 0x6E);
            patches.push(Patch {
                insn_offset: code.len(),
                kind: PatchKind::Method,
                old_idx: println_method,
            });
            code.push(0);
            let lo = (print_reg & 0x0F) | ((sb_reg & 0x0F) << 4);
            code.push(lo);

            2 // outs_size — every invoke-virtual passes ≤ 2 registers
        }
        CallKind::StaticJava { .. }
        | CallKind::Constructor(_)
        | CallKind::ConstructorJava { .. }
        | CallKind::Virtual { .. }
        | CallKind::Super { .. }
        | CallKind::VirtualJava { .. } => {
            // TODO: class support in DEX backend
            0
        }
    }
}

fn type_descriptor(ty: &Ty) -> &'static str {
    match ty {
        Ty::Unit => "V",
        Ty::Bool => "Z",
        Ty::Int => "I",
        Ty::Long => "J",
        Ty::Double => "D",
        Ty::String => "Ljava/lang/String;",
        Ty::IntArray => "[I",
        Ty::Any | Ty::Class(_) | Ty::Nullable(_) => "Ljava/lang/Object;",
        Ty::Function { .. } => "Ljava/lang/Object;",
        Ty::Nothing => "V", // Nothing → void (unreachable on DEX)
        Ty::Error => "V",
    }
}

// ─── opcode helpers ──────────────────────────────────────────────────────

/// Pack `vAA` into the high byte and `op` into the low byte of a
/// 16-bit code unit. Used by formats 21c, 21s, 31i, 22x, etc.
fn opcode_aa(op: u8, aa: u8) -> u16 {
    ((aa as u16) << 8) | op as u16
}

/// Format 10x: `00|op` — instructions with no register or literal.
fn opcode_10x(op: u8) -> u16 {
    op as u16
}
