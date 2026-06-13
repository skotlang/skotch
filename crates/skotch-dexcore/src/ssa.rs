//! SSA construction for the loop-capable IR path (the d8 IR pipeline).
//!
//! d8 builds an SSA IR, allocates registers with a linear scan, and resolves
//! φ-nodes into moves. To match its output on methods with control-flow merges
//! (loops, switches, ternaries) we need the same shape. This module is the front
//! of that pipeline: from the basic-block CFG it computes the dominator tree and
//! dominance frontiers, places φ-nodes for locals assigned in more than one
//! block (Cytron et al.), and renames locals into versioned SSA values.
//!
//! The operand-stack values produced by abstract interpretation are already
//! single-assignment; only JVM *locals* (which a loop reassigns across the
//! back-edge) need φ-nodes.
//!
//! WIP: the IR is built and structurally verified, but its consumer (live
//! intervals → linear scan → DexBuilder) is still being built, so many fields
//! are not yet read.
#![allow(dead_code)]

use crate::bootstrap::{split_blocks, Block};
use anyhow::{bail, Result};
use skotch_classfile::model::ClassFile;
use skotch_dex::model::{FieldRef, Fixup, ItemRef, MethodRef, ProtoRef};
use std::collections::{BTreeMap, BTreeSet};

// ───────────────────────────── SSA IR ─────────────────────────────

pub(crate) type ValId = u32;

/// An SSA value: produced either by a φ-node or a normal instruction. φ operands
/// are listed in predecessor order (parallel to the block's `preds`).
#[derive(Clone, Debug)]
pub(crate) enum SsaOp {
    /// A block-header φ for a local slot; operands parallel the block's preds.
    Phi { slot: u16, operands: Vec<ValId> },
    Argument { index: usize },
    ConstInt(i32),
    ConstLong(i64),
    ConstString(String),
    Binop { jvm_op: u8, a: ValId, b: ValId },
    Unop { jvm_op: u8, a: ValId },
    Cmp { jvm_op: u8, a: ValId, b: ValId },
    /// A method call: emits `invoke-* {args}` (+ `move-result` if non-void). The
    /// value's register is the move-result destination; void calls allocate no
    /// register. `ret` is the result kind, `None` for void. `jvm_pc` locates the
    /// source line for the throwing-instruction debug position.
    Invoke {
        dex_op: u16,
        method: MethodRef,
        args: Vec<ValId>,
        ret: Option<RetKind>,
        jvm_pc: u32,
    },
    /// Instance field read (`iget* dest, obj, field@`). Throwing (NPE).
    GetField { dex_op: u16, field: FieldRef, obj: ValId, jvm_pc: u32 },
    /// Static field read (`sget* dest, field@`). Throwing (class init).
    GetStatic { dex_op: u16, field: FieldRef, jvm_pc: u32 },
    /// Instance field write (`iput* value, obj, field@`). A statement (no result).
    PutField { dex_op: u16, field: FieldRef, obj: ValId, value: ValId, jvm_pc: u32 },
    /// Static field write (`sput* value, field@`). A statement (no result).
    PutStatic { dex_op: u16, field: FieldRef, value: ValId, jvm_pc: u32 },
    /// Array element read (`aget* dest, array, index`, 23x). Throwing (NPE/AIOOBE).
    ArrayGet { dex_op: u16, array: ValId, index: ValId, jvm_pc: u32 },
    /// Array element write (`aput* value, array, index`, 23x). A statement.
    ArrayPut { dex_op: u16, array: ValId, index: ValId, value: ValId, jvm_pc: u32 },
    /// Array length (`array-length dest, array`, 12x). Throwing (NPE).
    ArrayLength { array: ValId, jvm_pc: u32 },
}

/// The result kind of a non-void call (selects the `move-result` variant).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetKind {
    pub(crate) wide: bool,
    pub(crate) is_ref: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SsaValue {
    pub(crate) id: ValId,
    pub(crate) op: SsaOp,
    pub(crate) wide: bool,
    pub(crate) block: usize,
}

/// A block terminator (control flow leaving the block).
#[derive(Clone, Debug)]
pub(crate) enum Terminator {
    /// Conditional branch on `cond` operands (1 or 2) to `taken`, else fall to
    /// `fallthrough`.
    If { jvm_op: u8, operands: Vec<ValId>, taken: usize, fallthrough: usize },
    Goto { target: usize },
    Return(Option<ValId>),
    /// Falls through to the single successor with no explicit branch.
    Fall { target: usize },
}

#[derive(Clone, Debug)]
pub(crate) struct SsaBlock {
    /// φ value ids defined at this block's header (one per φ).
    pub(crate) phis: Vec<ValId>,
    /// Non-φ value ids defined in this block, in order.
    pub(crate) body: Vec<ValId>,
    pub(crate) term: Terminator,
    pub(crate) succ: Vec<usize>,
    pub(crate) preds: Vec<usize>,
}

pub(crate) struct SsaFn {
    pub(crate) values: Vec<SsaValue>,
    pub(crate) blocks: Vec<SsaBlock>,
    pub(crate) num_arg_registers: u16,
}

/// The control-flow graph: blocks with successor AND predecessor edges.
pub(crate) struct Cfg {
    pub(crate) blocks: Vec<Block>,
    pub(crate) preds: Vec<Vec<usize>>,
    /// Reverse-postorder (entry first); the order dominance iterates in.
    pub(crate) rpo: Vec<usize>,
}

impl Cfg {
    pub(crate) fn build(bc: &[u8]) -> Result<Cfg> {
        let blocks = split_blocks(bc)?;
        let n = blocks.len();
        let mut preds = vec![Vec::new(); n];
        for (b, blk) in blocks.iter().enumerate() {
            for &s in &blk.succ {
                preds[s].push(b);
            }
        }
        let rpo = reverse_postorder(&blocks);
        Ok(Cfg { blocks, preds, rpo })
    }

    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }
}

/// Reverse postorder from block 0 (a DFS finishing-order reversal).
fn reverse_postorder(blocks: &[Block]) -> Vec<usize> {
    let n = blocks.len();
    let mut visited = vec![false; n];
    let mut post = Vec::with_capacity(n);
    // Iterative DFS to avoid recursion on deep CFGs.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    visited[0] = true;
    while let Some(&(b, ref _i)) = stack.last() {
        let i = stack.last().unwrap().1;
        if i < blocks[b].succ.len() {
            stack.last_mut().unwrap().1 += 1;
            let s = blocks[b].succ[i];
            if !visited[s] {
                visited[s] = true;
                stack.push((s, 0));
            }
        } else {
            post.push(b);
            stack.pop();
        }
    }
    post.reverse();
    post
}

/// Immediate dominators (Cooper–Harvey–Kennedy). `idom[entry] == entry`;
/// unreachable blocks get `usize::MAX`.
pub(crate) fn dominators(cfg: &Cfg) -> Vec<usize> {
    let n = cfg.len();
    let mut idom = vec![usize::MAX; n];
    idom[0] = 0;
    // Postorder numbering for the intersect walk.
    let mut po_num = vec![usize::MAX; n];
    for (i, &b) in cfg.rpo.iter().rev().enumerate() {
        po_num[b] = i;
    }
    let intersect = |idom: &[usize], mut a: usize, mut b: usize| -> usize {
        while a != b {
            while po_num[a] < po_num[b] {
                a = idom[a];
            }
            while po_num[b] < po_num[a] {
                b = idom[b];
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &cfg.rpo {
            if b == 0 {
                continue;
            }
            let mut new_idom = usize::MAX;
            for &p in &cfg.preds[b] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new_idom = if new_idom == usize::MAX { p } else { intersect(&idom, p, new_idom) };
            }
            if new_idom != usize::MAX && idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }
    idom
}

/// Dominance frontiers (Cytron et al.).
pub(crate) fn dominance_frontiers(cfg: &Cfg, idom: &[usize]) -> Vec<BTreeSet<usize>> {
    let n = cfg.len();
    let mut df = vec![BTreeSet::new(); n];
    for b in 0..n {
        if cfg.preds[b].len() < 2 {
            continue;
        }
        for &p in &cfg.preds[b] {
            let mut runner = p;
            while runner != usize::MAX && runner != idom[b] {
                df[runner].insert(b);
                if runner == idom[runner] {
                    break;
                }
                runner = idom[runner];
            }
        }
    }
    df
}

/// The set of local slots written in each block (the φ def-sites). A wide store
/// (long/double) writes two slots.
pub(crate) fn def_sites(cfg: &Cfg, bc: &[u8]) -> BTreeMap<u16, BTreeSet<usize>> {
    let mut sites: BTreeMap<u16, BTreeSet<usize>> = BTreeMap::new();
    for (bi, blk) in cfg.blocks.iter().enumerate() {
        let mut pc = blk.start;
        while pc < blk.end {
            if let Some((slot, len)) = crate::bootstrap::store_slot(bc, pc) {
                sites.entry(slot as u16).or_default().insert(bi);
                if crate::bootstrap::store_is_wide(bc[pc]) {
                    sites.entry(slot as u16 + 1).or_default().insert(bi);
                }
                pc += len;
            } else if bc[pc] == 0x84 {
                // iinc index, const — increments a local in place (a def + use).
                sites.entry(bc[pc + 1] as u16).or_default().insert(bi);
                pc += 3;
            } else {
                pc += crate::bootstrap::jvm_step_len(bc, pc);
            }
        }
    }
    sites
}

/// True if the method body contains a back-edge (a branch whose target precedes
/// it), i.e. a loop. Loop methods need the SSA/φ pipeline; acyclic methods are
/// served byte-identically by the bootstrap straight-line / CFG paths.
pub(crate) fn method_has_loop(bc: &[u8]) -> bool {
    let mut pc = 0usize;
    while pc < bc.len() {
        let op = bc[pc];
        match op {
            // conditional branches, goto, jsr, ifnull/ifnonnull — 2-byte offset.
            0x99..=0xa8 | 0xc6 | 0xc7 => {
                if i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) < 0 {
                    return true;
                }
                pc += 3;
            }
            // goto_w / jsr_w — 4-byte offset.
            0xc8 | 0xc9 => {
                if i32::from_be_bytes([bc[pc + 1], bc[pc + 2], bc[pc + 3], bc[pc + 4]]) < 0 {
                    return true;
                }
                pc += 5;
            }
            // tableswitch — variable length, padded to a 4-byte boundary.
            0xaa => {
                let base = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let rd = |i: usize| i32::from_be_bytes([bc[i], bc[i + 1], bc[i + 2], bc[i + 3]]);
                let (default, low, high) = (rd(base), rd(base + 4), rd(base + 8));
                let n = (high - low + 1) as usize;
                let jumps = base + 12;
                if default < 0 || (0..n).any(|k| rd(jumps + 4 * k) < 0) {
                    return true;
                }
                pc = jumps + 4 * n;
            }
            // lookupswitch — variable length, padded to a 4-byte boundary.
            0xab => {
                let base = pc + 1 + (4 - ((pc + 1) % 4)) % 4;
                let rd = |i: usize| i32::from_be_bytes([bc[i], bc[i + 1], bc[i + 2], bc[i + 3]]);
                let (default, npairs) = (rd(base), rd(base + 4) as usize);
                let pairs = base + 8;
                if default < 0 || (0..npairs).any(|k| rd(pairs + 8 * k + 4) < 0) {
                    return true;
                }
                pc = pairs + 8 * npairs;
            }
            _ => pc += crate::bootstrap::jvm_step_len(bc, pc),
        }
    }
    false
}

/// Builds the SSA form of a method body (loop-capable). Handles the integer
/// loop/branch subset (loads, int constants, iinc, int binops, comparisons,
/// conditional branches, gotos, returns); bails on anything else for now.
pub(crate) fn build_ssa(
    cf: &ClassFile,
    bc: &[u8],
    params: &[String],
    instance: bool,
) -> Result<SsaFn> {
    let cfg = Cfg::build(bc)?;
    let n = cfg.len();
    let idom = dominators(&cfg);
    let df = dominance_frontiers(&cfg, &idom);
    let sites = def_sites(&cfg, bc);

    // Argument slots (wide args take two; only the low slot names the value).
    let mut arg_slots = Vec::new();
    let mut num_arg_registers = 0u16;
    {
        let mut slot = 0u16;
        if instance {
            arg_slots.push(slot);
            slot += 1;
            num_arg_registers += 1;
        }
        for p in params {
            arg_slots.push(slot);
            let wide = p == "J" || p == "D";
            slot += if wide { 2 } else { 1 };
            num_arg_registers += if wide { 2 } else { 1 };
        }
    }
    let phis = phi_blocks(&df, &sites, &arg_slots);

    let mut values: Vec<SsaValue> = Vec::new();
    let mut blocks: Vec<SsaBlock> = (0..n)
        .map(|b| SsaBlock {
            phis: Vec::new(),
            body: Vec::new(),
            term: Terminator::Return(None),
            succ: cfg.blocks[b].succ.clone(),
            preds: cfg.preds[b].clone(),
        })
        .collect();

    let mut b = Builder { values: &mut values };

    // Pre-create argument values and the initial slot→version mapping.
    let mut arg_value: BTreeMap<u16, ValId> = BTreeMap::new();
    {
        let mut idx = 0usize;
        let mut slot = 0u16;
        if instance {
            arg_value.insert(slot, b.new(SsaOp::Argument { index: idx }, false, 0));
            idx += 1;
            slot += 1;
        }
        for p in params {
            let wide = p == "J" || p == "D";
            arg_value.insert(slot, b.new(SsaOp::Argument { index: idx }, wide, 0));
            idx += 1;
            slot += if wide { 2 } else { 1 };
        }
    }

    // Create φ value ids per block (operands filled during renaming). Track which
    // slot each φ is for so renaming can wire predecessor operands.
    let mut block_phi_slots: Vec<Vec<u16>> = vec![Vec::new(); n];
    for (&slot, bset) in &phis {
        for &blk in bset {
            let id = b.new(SsaOp::Phi { slot, operands: Vec::new() }, false, blk);
            blocks[blk].phis.push(id);
            block_phi_slots[blk].push(slot);
        }
    }

    // Dominator-tree children.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for blk in 0..n {
        if blk != 0 && idom[blk] != usize::MAX {
            children[idom[blk]].push(blk);
        }
    }

    // Version stacks per slot, seeded with argument values.
    let mut versions: BTreeMap<u16, Vec<ValId>> = BTreeMap::new();
    for (&slot, &v) in &arg_value {
        versions.entry(slot).or_default().push(v);
    }

    rename(cf, &cfg, bc, &children, &block_phi_slots, &mut blocks, &mut b, &mut versions, 0)?;

    Ok(SsaFn { values, blocks, num_arg_registers })
}

struct Builder<'a> {
    values: &'a mut Vec<SsaValue>,
}
impl<'a> Builder<'a> {
    fn new(&mut self, op: SsaOp, wide: bool, block: usize) -> ValId {
        let id = self.values.len() as ValId;
        self.values.push(SsaValue { id, op, wide, block });
        id
    }
}

#[allow(clippy::too_many_arguments)]
fn rename(
    cf: &ClassFile,
    cfg: &Cfg,
    bc: &[u8],
    children: &[Vec<usize>],
    block_phi_slots: &[Vec<u16>],
    blocks: &mut [SsaBlock],
    b: &mut Builder,
    versions: &mut BTreeMap<u16, Vec<ValId>>,
    blk: usize,
) -> Result<()> {
    // Track how many versions we push for this block, to pop on the way out.
    let mut pushed: Vec<u16> = Vec::new();

    // φ outputs become the current version of their slot.
    for (k, &slot) in block_phi_slots[blk].iter().enumerate() {
        let id = blocks[blk].phis[k];
        versions.entry(slot).or_default().push(id);
        pushed.push(slot);
    }

    // Abstract-interpret the block (operand stack empty at entry).
    let mut stack: Vec<ValId> = Vec::new();
    let cur = |versions: &BTreeMap<u16, Vec<ValId>>, slot: u16| -> Result<ValId> {
        versions
            .get(&slot)
            .and_then(|s| s.last().copied())
            .ok_or_else(|| anyhow::anyhow!("ssa: use of undefined local slot {slot}"))
    };
    let (start, end) = (cfg.blocks[blk].start, cfg.blocks[blk].end);
    let mut pc = start;
    let mut term: Option<Terminator> = None;
    while pc < end {
        let op = bc[pc];
        match op {
            // loads
            0x1a..=0x1d => { stack.push(cur(versions, (op - 0x1a) as u16)?); pc += 1; }
            0x1e..=0x21 => { stack.push(cur(versions, (op - 0x1e) as u16)?); pc += 1; }
            0x22..=0x25 => { stack.push(cur(versions, (op - 0x22) as u16)?); pc += 1; }
            0x26..=0x29 => { stack.push(cur(versions, (op - 0x26) as u16)?); pc += 1; }
            0x2a..=0x2d => { stack.push(cur(versions, (op - 0x2a) as u16)?); pc += 1; }
            0x15..=0x19 => { stack.push(cur(versions, bc[pc + 1] as u16)?); pc += 2; }
            // int constants
            0x02..=0x08 => { let v = b.new(SsaOp::ConstInt(op as i32 - 0x03), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x10 => { let v = b.new(SsaOp::ConstInt(bc[pc + 1] as i8 as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 2; }
            0x11 => { let v = b.new(SsaOp::ConstInt(i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 3; }
            // stores: rename the slot to the popped value (no instruction)
            0x36..=0x3a => { let v = stack.pop().unwrap(); versions.entry(bc[pc + 1] as u16).or_default().push(v); pushed.push(bc[pc + 1] as u16); pc += 2; }
            0x3b..=0x3e => { let v = stack.pop().unwrap(); let s = (op - 0x3b) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x3f..=0x42 => { let v = stack.pop().unwrap(); let s = (op - 0x3f) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x43..=0x46 => { let v = stack.pop().unwrap(); let s = (op - 0x43) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x47..=0x4a => { let v = stack.pop().unwrap(); let s = (op - 0x47) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x4b..=0x4e => { let v = stack.pop().unwrap(); let s = (op - 0x4b) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            // iinc slot, const → slot = slot + const
            0x84 => {
                let slot = bc[pc + 1] as u16;
                let c = bc[pc + 2] as i8 as i32;
                let cst = b.new(SsaOp::ConstInt(c), false, blk);
                blocks[blk].body.push(cst);
                let old = cur(versions, slot)?;
                let sum = b.new(SsaOp::Binop { jvm_op: 0x60, a: old, b: cst }, false, blk);
                blocks[blk].body.push(sum);
                versions.entry(slot).or_default().push(sum);
                pushed.push(slot);
                pc += 3;
            }
            // int binops
            0x60 | 0x64 | 0x68 | 0x6c | 0x70 | 0x7e | 0x80 | 0x82 | 0x78 | 0x7a | 0x7c => {
                let rb = stack.pop().unwrap();
                let ra = stack.pop().unwrap();
                let v = b.new(SsaOp::Binop { jvm_op: op, a: ra, b: rb }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // comparisons (produce a narrow result, used by a following branch)
            0x94..=0x98 => {
                let rb = stack.pop().unwrap();
                let ra = stack.pop().unwrap();
                let v = b.new(SsaOp::Cmp { jvm_op: op, a: ra, b: rb }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // conditional branches
            0x99..=0xa4 => {
                let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                let two = (0x9f..=0xa4).contains(&op);
                let operands = if two {
                    let r = stack.pop().unwrap();
                    let l = stack.pop().unwrap();
                    vec![l, r]
                } else {
                    vec![stack.pop().unwrap()]
                };
                let taken = cfg.blocks.iter().position(|x| x.start == target).unwrap();
                let fallthrough = *cfg.blocks[blk].succ.iter().find(|&&s| s != taken).unwrap_or(&taken);
                term = Some(Terminator::If { jvm_op: op, operands, taken, fallthrough });
                pc += 3;
            }
            0xa7 => {
                let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                let t = cfg.blocks.iter().position(|x| x.start == target).unwrap();
                term = Some(Terminator::Goto { target: t });
                pc += 3;
            }
            0xb1 => { term = Some(Terminator::Return(None)); pc += 1; }
            0xac | 0xad | 0xae | 0xaf | 0xb0 => { term = Some(Terminator::Return(Some(stack.pop().unwrap()))); pc += 1; }
            // method calls: invokevirtual/special/static/interface
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let (class, name, desc) = cf.constant_pool.member_ref(idx)?;
                let (mparams, ret) = crate::bootstrap::parse_descriptor(&desc)?;
                let is_static = op == 0xb8;
                let argc = mparams.len() + if is_static { 0 } else { 1 };
                let mut args: Vec<ValId> = Vec::with_capacity(argc);
                for _ in 0..argc {
                    args.push(stack.pop().unwrap());
                }
                args.reverse();
                let dex_op: u16 = match op {
                    0xb6 => 0x6e,                                          // invoke-virtual
                    0xb7 => if name == "<init>" { 0x70 } else { 0x6f },    // invoke-direct/super
                    0xb8 => 0x71,                                          // invoke-static
                    0xb9 => 0x72,                                          // invoke-interface
                    _ => unreachable!(),
                };
                let method = MethodRef {
                    class: skotch_classfile::constant_pool::internal_to_descriptor(&class),
                    proto: ProtoRef { return_type: ret.clone(), params: mparams },
                    name,
                };
                let ret_kind = if ret == "V" {
                    None
                } else {
                    Some(RetKind { wide: ret == "J" || ret == "D", is_ref: crate::bootstrap::is_ref(&ret) })
                };
                let wide = ret_kind.map(|r| r.wide).unwrap_or(false);
                let v = b.new(
                    SsaOp::Invoke { dex_op, method, args, ret: ret_kind, jvm_pc: pc as u32 },
                    wide,
                    blk,
                );
                blocks[blk].body.push(v);
                if ret_kind.is_some() {
                    stack.push(v);
                }
                pc += if op == 0xb9 { 5 } else { 3 };
            }
            // field access: getstatic/putstatic/getfield/putfield
            0xb2 | 0xb3 | 0xb4 | 0xb5 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let (class, name, desc) = cf.constant_pool.member_ref(idx)?;
                let field = FieldRef {
                    class: skotch_classfile::constant_pool::internal_to_descriptor(&class),
                    type_: desc.clone(),
                    name,
                };
                let wide = desc == "J" || desc == "D";
                match op {
                    0xb2 => {
                        let v = b.new(SsaOp::GetStatic { dex_op: crate::bootstrap::sget_op(&desc), field, jvm_pc: pc as u32 }, wide, blk);
                        blocks[blk].body.push(v);
                        stack.push(v);
                    }
                    0xb4 => {
                        let obj = stack.pop().unwrap();
                        let v = b.new(SsaOp::GetField { dex_op: crate::bootstrap::iget_op(&desc), field, obj, jvm_pc: pc as u32 }, wide, blk);
                        blocks[blk].body.push(v);
                        stack.push(v);
                    }
                    0xb3 => {
                        let value = stack.pop().unwrap();
                        let v = b.new(SsaOp::PutStatic { dex_op: crate::bootstrap::sput_op(&desc), field, value, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(v);
                    }
                    0xb5 => {
                        let value = stack.pop().unwrap();
                        let obj = stack.pop().unwrap();
                        let v = b.new(SsaOp::PutField { dex_op: crate::bootstrap::iput_op(&desc), field, obj, value, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(v);
                    }
                    _ => unreachable!(),
                }
                pc += 3;
            }
            // array element load: iaload/laload/faload/daload/aaload/baload/caload/saload
            0x2e..=0x35 => {
                let (dex_op, wide) = crate::bootstrap::aget_op(op);
                let index = stack.pop().unwrap();
                let array = stack.pop().unwrap();
                let v = b.new(SsaOp::ArrayGet { dex_op, array, index, jvm_pc: pc as u32 }, wide, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // array element store: i/l/f/d/a/b/c/sastore
            0x4f..=0x56 => {
                let dex_op = crate::bootstrap::aput_op(op);
                let value = stack.pop().unwrap();
                let index = stack.pop().unwrap();
                let array = stack.pop().unwrap();
                let v = b.new(SsaOp::ArrayPut { dex_op, array, index, value, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                pc += 1;
            }
            // arraylength
            0xbe => {
                let array = stack.pop().unwrap();
                let v = b.new(SsaOp::ArrayLength { array, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            other => bail!("ssa: unsupported opcode {other:#04x} (loop subset only)"),
        }
    }
    if !stack.is_empty() {
        bail!("ssa: non-empty operand stack at block boundary (needs stack-merge φ)");
    }
    // A block with no explicit terminator falls through to its single successor.
    blocks[blk].term = term.unwrap_or_else(|| {
        Terminator::Fall { target: cfg.blocks[blk].succ.first().copied().unwrap_or(blk) }
    });

    // Fill φ operands in successors for this predecessor edge.
    let succ = cfg.blocks[blk].succ.clone();
    for &s in &succ {
        let pred_idx = cfg.preds[s].iter().position(|&p| p == blk).unwrap();
        let slots = block_phi_slots[s].clone();
        for (k, &slot) in slots.iter().enumerate() {
            let phi_id = blocks[s].phis[k];
            let operand = cur(versions, slot)?;
            if let SsaOp::Phi { operands, .. } = &mut b.values[phi_id as usize].op {
                if operands.len() <= pred_idx {
                    operands.resize(pred_idx + 1, operand);
                }
                operands[pred_idx] = operand;
            }
        }
    }

    // Recurse into dominator-tree children.
    for &c in &children[blk] {
        rename(cf, cfg, bc, children, block_phi_slots, blocks, b, versions, c)?;
    }

    // Pop versions defined in this block.
    for slot in pushed.into_iter().rev() {
        versions.get_mut(&slot).unwrap().pop();
    }
    Ok(())
}

// ──────────────────── numbering + live intervals ────────────────────

/// Instruction numbering (d8 uses a step of 2). Each SSA value gets a `def`
/// number; the block layout (here, block-index order) determines the linear
/// positions used for liveness and the back-edge extension.
pub(crate) struct Numbering {
    /// def position per value id.
    pub(crate) def: Vec<u32>,
    /// [first_number, last_number) per block, in layout order.
    pub(crate) block_span: Vec<(u32, u32)>,
    /// layout order of blocks (block indices).
    pub(crate) layout: Vec<usize>,
}

pub(crate) const NUMBER_DELTA: u32 = 2;

/// Numbers φ-nodes at each block header (all share the block's entry number, as
/// in d8) and the body instructions sequentially.
pub(crate) fn number(f: &SsaFn) -> Numbering {
    let layout: Vec<usize> = (0..f.blocks.len()).collect();
    let mut def = vec![0u32; f.values.len()];
    let mut block_span = vec![(0u32, 0u32); f.blocks.len()];
    let mut next = 0u32;
    for &b in &layout {
        let first = next;
        // φ-nodes live at the block entry number.
        for &p in &f.blocks[b].phis {
            def[p as usize] = next;
        }
        // The entry "slot" (where φ moves land) consumes one number.
        next += NUMBER_DELTA;
        for &v in &f.blocks[b].body {
            def[v as usize] = next;
            next += NUMBER_DELTA;
        }
        block_span[b] = (first, next);
    }
    Numbering { def, block_span, layout }
}

/// A value's live range: [start, end). Loop-carried values get a single range
/// extended to cover the whole loop via the back-edge liveness.
#[derive(Clone, Debug)]
pub(crate) struct Interval {
    pub(crate) value: ValId,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

/// Computes per-value live intervals. live-in/out are found by backward dataflow
/// over the CFG (including back-edges, so loop-carried values stay live across
/// the whole loop); intervals then span each value's def to its last live point.
pub(crate) fn live_intervals(f: &SsaFn, num: &Numbering) -> Vec<Interval> {
    let n = f.blocks.len();
    // uses[b] = values used in b before any (local) def; defs[b] = values defined.
    let mut use_: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    let mut def_: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    for b in 0..n {
        let blk = &f.blocks[b];
        let mut defined: BTreeSet<ValId> = BTreeSet::new();
        // φ outputs are defined at entry; their OPERANDS are uses in the
        // predecessor blocks (handled below as live-out contributions).
        for &p in &blk.phis {
            defined.insert(p);
            def_[b].insert(p);
        }
        for &v in &blk.body {
            for u in operands(&f.values[v as usize].op) {
                if !defined.contains(&u) {
                    use_[b].insert(u);
                }
            }
            defined.insert(v);
            def_[b].insert(v);
        }
        for u in term_operands(&blk.term) {
            if !defined.contains(&u) {
                use_[b].insert(u);
            }
        }
    }
    // Backward dataflow to a fixpoint. A φ operand is live-out of exactly the
    // predecessor it comes from (not a general block use).
    let mut live_in: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    let mut live_out: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for b in (0..n).rev() {
            let mut lo: BTreeSet<ValId> = BTreeSet::new();
            for &s in &f.blocks[b].succ {
                // successor live-in minus its φ outputs (those don't flow back).
                for &v in &live_in[s] {
                    lo.insert(v);
                }
                // plus this edge's φ operands.
                let pred_idx = f.blocks[s].preds.iter().position(|&p| p == b).unwrap();
                for &phi in &f.blocks[s].phis {
                    if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                        if let Some(&opnd) = operands.get(pred_idx) {
                            lo.insert(opnd);
                        }
                    }
                }
            }
            let mut li = use_[b].clone();
            for &v in &lo {
                if !def_[b].contains(&v) {
                    li.insert(v);
                }
            }
            if lo != live_out[b] {
                live_out[b] = lo;
                changed = true;
            }
            if li != live_in[b] {
                live_in[b] = li;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Build intervals: for each value, [def, last point it's live]. A value
    // live-out of a block extends to that block's end; live across a loop body
    // (live-in at the header and live-out of the back-edge block) yields one
    // range covering the loop.
    let mut start: BTreeMap<ValId, u32> = BTreeMap::new();
    let mut end: BTreeMap<ValId, u32> = BTreeMap::new();
    let note = |v: ValId, lo: u32, hi: u32, s: &mut BTreeMap<ValId, u32>, e: &mut BTreeMap<ValId, u32>| {
        s.entry(v).and_modify(|x| *x = (*x).min(lo)).or_insert(lo);
        e.entry(v).and_modify(|x| *x = (*x).max(hi)).or_insert(hi);
    };
    for b in 0..n {
        let (bstart, bend) = num.block_span[b];
        // Values live through the whole block (live-in ∩ live-out).
        for &v in live_in[b].intersection(&live_out[b]) {
            note(v, bstart, bend, &mut start, &mut end);
        }
        // Definitions: from def to block end if live-out, else to last use.
        for &v in &f.blocks[b].phis {
            note(v, bstart, bstart, &mut start, &mut end);
        }
        for &v in &f.blocks[b].body {
            // A value with no result (a void call) reserves no register.
            if !produces_value(&f.values[v as usize].op) {
                continue;
            }
            let d = num.def[v as usize];
            note(v, d, d, &mut start, &mut end);
            if live_out[b].contains(&v) {
                note(v, d, bend, &mut start, &mut end);
            }
        }
        // Uses extend the interval to the use point.
        let mut pos = bstart + NUMBER_DELTA;
        for &v in &f.blocks[b].body {
            for u in operands(&f.values[v as usize].op) {
                note(u, num.def.get(u as usize).copied().unwrap_or(0), pos, &mut start, &mut end);
            }
            pos += NUMBER_DELTA;
        }
        for u in term_operands(&f.blocks[b].term) {
            note(u, num.def.get(u as usize).copied().unwrap_or(0), bend, &mut start, &mut end);
        }
        // live-in values extend back to block start.
        for &v in &live_in[b] {
            note(v, bstart, bstart, &mut start, &mut end);
        }
    }
    let mut intervals: Vec<Interval> = start
        .keys()
        .map(|&v| Interval { value: v, start: start[&v], end: end[&v] })
        .collect();
    intervals.sort_by_key(|iv| (iv.start, iv.value));
    intervals
}

/// The value operands an op reads.
fn operands(op: &SsaOp) -> Vec<ValId> {
    match op {
        SsaOp::Phi { .. } | SsaOp::Argument { .. } | SsaOp::ConstInt(_) | SsaOp::ConstLong(_) | SsaOp::ConstString(_) => Vec::new(),
        SsaOp::Binop { a, b, .. } | SsaOp::Cmp { a, b, .. } => vec![*a, *b],
        SsaOp::Unop { a, .. } => vec![*a],
        SsaOp::Invoke { args, .. } => args.clone(),
        SsaOp::GetStatic { .. } => Vec::new(),
        SsaOp::GetField { obj, .. } => vec![*obj],
        SsaOp::PutStatic { value, .. } => vec![*value],
        SsaOp::PutField { obj, value, .. } => vec![*obj, *value],
        SsaOp::ArrayGet { array, index, .. } => vec![*array, *index],
        SsaOp::ArrayPut { array, index, value, .. } => vec![*array, *index, *value],
        SsaOp::ArrayLength { array, .. } => vec![*array],
    }
}

/// Whether an op defines a result that needs a register. A void call, a field
/// store, or an array store defines no value (it's a pure side-effect statement).
fn produces_value(op: &SsaOp) -> bool {
    !matches!(
        op,
        SsaOp::Invoke { ret: None, .. }
            | SsaOp::PutField { .. }
            | SsaOp::PutStatic { .. }
            | SsaOp::ArrayPut { .. }
    )
}

fn term_operands(t: &Terminator) -> Vec<ValId> {
    match t {
        Terminator::If { operands, .. } => operands.clone(),
        Terminator::Return(Some(v)) => vec![*v],
        _ => Vec::new(),
    }
}

// ──────────────────── linear-scan register allocation ────────────────────

/// Per-value register assignment in d8's "allocated space" (args at
/// `[0, num_arg_registers)`; the allocated→real args-high remap is applied later
/// by `crate::regalloc`). Coalesced values (a φ and its operands; an in-place
/// update and its source) share a register so loop-carried values need no moves.
pub(crate) struct Allocation {
    /// allocated register per value id (NO_REG for rematerialized constants).
    pub(crate) reg: Vec<u16>,
    pub(crate) registers_used: u16,
}

pub(crate) const NO_REG: u16 = u16::MAX;

/// Whether a constant value is *rematerialized* (folded as a literal) rather
/// than allocated: a small int constant used only by lit-foldable ops, mirroring
/// d8's const handling (`iinc`'s constant becomes `add-int/lit8`, no register).
fn is_rematerialized(f: &SsaFn, v: ValId) -> bool {
    let val = &f.values[v as usize];
    let c = match val.op {
        SsaOp::ConstInt(c) => c,
        _ => return false,
    };
    if !(-128..=127).contains(&c) {
        return false;
    }
    // Every use must be a lit-foldable binop with this constant as the RIGHT operand.
    let mut any_use = false;
    for u in &f.values {
        let (jop, a, b) = match u.op {
            SsaOp::Binop { jvm_op, a, b } => (jvm_op, a, b),
            _ => continue,
        };
        if a == v {
            return false; // constant as left operand can't lit-fold
        }
        if b == v {
            any_use = true;
            if crate::bootstrap::lit_ops(jop).is_none() {
                return false;
            }
        }
    }
    any_use
}

/// Union–find over values that must share a register (coalescing).
struct Coalesce {
    parent: Vec<u32>,
}
impl Coalesce {
    fn new(n: usize) -> Coalesce {
        Coalesce { parent: (0..n as u32).collect() }
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let gp = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = gp; // path halving
            x = gp;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // Keep the lower id as root (deterministic).
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

pub(crate) fn allocate(f: &SsaFn, num: &Numbering, intervals: &[Interval]) -> Allocation {
    let nv = f.values.len();
    // 1. Coalesce φ-nodes with their operands (loop-carried values share a reg).
    let mut co = Coalesce::new(nv);
    for v in &f.values {
        if let SsaOp::Phi { operands, .. } = &v.op {
            for &o in operands {
                co.union(v.id, o);
            }
        }
    }

    // 2. Pre-color arguments to allocated registers [0, num_arg).
    let mut reg = vec![NO_REG; nv];
    {
        let mut r = 0u16;
        for v in &f.values {
            if let SsaOp::Argument { .. } = v.op {
                reg[co.find(v.id) as usize] = r;
                r += if v.wide { 2 } else { 1 };
            }
        }
    }
    let num_arg = f.num_arg_registers;

    // 3. Linear scan over coalescing-group leaders, by interval start.
    //    Group interval = union of members' [start,end). Rematerialized
    //    constants get no register.
    let mut group_iv: BTreeMap<u32, (u32, u32, bool)> = BTreeMap::new(); // leader -> (start,end,wide)
    for iv in intervals {
        if is_rematerialized(f, iv.value) {
            continue;
        }
        let leader = co.find(iv.value);
        let wide = f.values[iv.value as usize].wide;
        let e = group_iv.entry(leader).or_insert((iv.start, iv.end, wide));
        e.0 = e.0.min(iv.start);
        e.1 = e.1.max(iv.end);
        e.2 |= wide;
    }
    let mut order: Vec<u32> = group_iv.keys().copied().collect();
    order.sort_by_key(|&g| (group_iv[&g].0, g));

    // Active groups: (end, leader, reg, wide).
    let mut active: Vec<(u32, u32, u16, bool)> = Vec::new();
    let mut max_reg: i32 = num_arg as i32 - 1;
    for &g in &order {
        let (start, end, wide) = group_iv[&g];
        // Pre-colored (argument) groups: already have a register.
        if reg[g as usize] != NO_REG {
            active.push((end, g, reg[g as usize], wide));
            continue;
        }
        // Expire groups that ended at/before this start.
        let mut occupied = vec![false; (max_reg + 2).max(num_arg as i32 + 2) as usize + 2];
        active.retain(|&(e, _, _, _)| e > start);
        for &(_, _, r, w) in &active {
            occupied[r as usize] = true;
            if w {
                occupied[r as usize + 1] = true;
            }
        }
        // Lowest free allocated register (a pair if wide), not straddling args.
        let need = if wide { 2 } else { 1 };
        let mut r = 0usize;
        loop {
            if r + need > occupied.len() {
                occupied.resize(r + need, false);
            }
            let straddle = wide && num_arg > 0 && r == (num_arg as usize - 1);
            if !straddle && (0..need).all(|k| !occupied[r + k]) {
                break;
            }
            r += 1;
        }
        reg[g as usize] = r as u16;
        max_reg = max_reg.max((r + need - 1) as i32);
        active.push((end, g, r as u16, wide));
    }

    // 4. Propagate group registers to all members.
    for v in 0..nv as u32 {
        if is_rematerialized(f, v) {
            continue;
        }
        let leader = co.find(v);
        reg[v as usize] = reg[leader as usize];
    }

    let _ = num;
    Allocation { reg, registers_used: (max_reg + 1).max(num_arg as i32) as u16 }
}

// ──────────────────────────── DexBuilder ────────────────────────────

use skotch_dex::model::CodeItem;

/// Full IR pipeline for a method body: SSA construction → numbering → live
/// intervals → linear-scan allocation → DexBuilder.
pub(crate) fn dex_method_ssa(
    cf: &ClassFile,
    bc: &[u8],
    params: &[String],
    instance: bool,
    line_numbers: &[(u16, u16)],
) -> Result<CodeItem> {
    let mut f = build_ssa(cf, bc, params, instance)?;
    // d8 builds its IR with lazily-created φ-nodes: a loop variable's φ is created
    // the first time its slot is *read*, so the φ for the variable used earliest in
    // the loop (the counter, read in the condition) gets the lower SSA number — and
    // hence the lower register. d8 then schedules each φ's entry initializer in that
    // same order. Our φ placement is by slot, so re-derive d8's order from a
    // preliminary numbering and reorder the entry initializers to match.
    let num0 = number(&f);
    let ranks = phi_first_use(&f, &num0);
    reorder_entry_inits(&mut f, &ranks);
    let num = number(&f);
    let ivs = live_intervals(&f, &num);
    let alloc = allocate(&f, &num, &ivs);
    build_dex(&f, &num, &alloc, line_numbers, params)
}

/// For each φ value, the earliest numbered position at which it is used (its
/// "first read" in d8's lazy-SSA construction order). Used to order the φs — and
/// their entry initializers — the way d8 does.
fn phi_first_use(f: &SsaFn, num: &Numbering) -> BTreeMap<ValId, u32> {
    let is_phi = |v: ValId| matches!(f.values[v as usize].op, SsaOp::Phi { .. });
    let mut first: BTreeMap<ValId, u32> = BTreeMap::new();
    let note = |phi: ValId, pos: u32, m: &mut BTreeMap<ValId, u32>| {
        m.entry(phi).and_modify(|x| *x = (*x).min(pos)).or_insert(pos);
    };
    for b in 0..f.blocks.len() {
        for &v in &f.blocks[b].body {
            let pos = num.def[v as usize];
            for o in operands(&f.values[v as usize].op) {
                if is_phi(o) {
                    note(o, pos, &mut first);
                }
            }
        }
        let bend = num.block_span[b].1;
        for o in term_operands(&f.blocks[b].term) {
            if is_phi(o) {
                note(o, bend, &mut first);
            }
        }
        // A φ operand contributed across an edge is read at the end of this block
        // (the back-edge move).
        for &s in &f.blocks[b].succ {
            let pred_idx = match f.blocks[s].preds.iter().position(|&p| p == b) {
                Some(i) => i,
                None => continue,
            };
            for &phi in &f.blocks[s].phis {
                if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                    if let Some(&o) = operands.get(pred_idx) {
                        if is_phi(o) {
                            note(o, bend, &mut first);
                        }
                    }
                }
            }
        }
    }
    first
}

/// Reorders, within each block, the constant entry-initializers that feed loop
/// φ-nodes so they appear in φ first-use order (matching d8's scheduling). Only
/// permutes pure constants among the positions they already occupy, so it never
/// crosses a data dependency.
fn reorder_entry_inits(f: &mut SsaFn, ranks: &BTreeMap<ValId, u32>) {
    let nb = f.blocks.len();
    for b in 0..nb {
        // value → rank-of-the-φ-it-initializes, for body values of this block that
        // are the entry-edge operand of some successor φ.
        let mut init_rank: BTreeMap<ValId, u32> = BTreeMap::new();
        for &s in &f.blocks[b].succ.clone() {
            let pred_idx = match f.blocks[s].preds.iter().position(|&p| p == b) {
                Some(i) => i,
                None => continue,
            };
            for &phi in &f.blocks[s].phis.clone() {
                if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                    if let Some(&o) = operands.get(pred_idx) {
                        if f.values[o as usize].block == b {
                            if let Some(&r) = ranks.get(&phi) {
                                init_rank.entry(o).and_modify(|x| *x = (*x).min(r)).or_insert(r);
                            }
                        }
                    }
                }
            }
        }
        if init_rank.len() < 2 {
            continue;
        }
        // Only reorder pure constants (leaves with no operands) — safe to permute.
        let all_const = init_rank.keys().all(|&v| {
            matches!(
                f.values[v as usize].op,
                SsaOp::ConstInt(_) | SsaOp::ConstLong(_) | SsaOp::ConstString(_)
            )
        });
        if !all_const {
            continue;
        }
        let body = &mut f.blocks[b].body;
        let positions: Vec<usize> =
            body.iter().enumerate().filter(|(_, &v)| init_rank.contains_key(&v)).map(|(i, _)| i).collect();
        let mut items: Vec<ValId> = positions.iter().map(|&i| body[i]).collect();
        items.sort_by_key(|v| (init_rank[v], *v));
        for (k, &pos) in positions.iter().enumerate() {
            body[pos] = items[k];
        }
    }
}

/// Emits a DEX code item from the SSA form + allocation. φ-nodes are no-ops when
/// coalesced (their value already lives in the shared register). Registers are
/// emitted in allocated space and then remapped args-high by `crate::regalloc`.
pub(crate) fn build_dex(
    f: &SsaFn,
    num: &Numbering,
    alloc: &Allocation,
    line_numbers: &[(u16, u16)],
    params: &[String],
) -> Result<CodeItem> {
    let mut insns: Vec<u16> = Vec::new();
    let mut block_unit = vec![0usize; f.blocks.len()];
    // (offset_word_index, target_block, is_goto)
    let mut fixups: Vec<(usize, usize, bool)> = Vec::new();
    // Constant-pool references (method/field) patched by the writer.
    let mut pool_fixups: Vec<Fixup> = Vec::new();
    // (dex_addr, line) at throwing instructions, for the debug_info.
    let mut positions: Vec<(u32, u32)> = Vec::new();
    let mut outs: u16 = 0;
    let reg = |v: ValId| alloc.reg[v as usize];

    for (pos, &b) in num.layout.iter().enumerate() {
        block_unit[b] = insns.len();
        // φ resolution: a no-op when every operand shares the φ's register.
        for &phi in &f.blocks[b].phis {
            let pr = reg(phi);
            if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                if operands.iter().any(|&o| reg(o) != pr) {
                    bail!("ssa dexbuilder: non-coalesced φ needs moves (not yet supported)");
                }
            }
        }
        for &v in &f.blocks[b].body {
            if is_rematerialized(f, v) {
                continue;
            }
            match &f.values[v as usize].op {
                SsaOp::Invoke { .. } => {
                    emit_invoke(f, &mut insns, alloc, v, &mut pool_fixups, &mut outs, &mut positions, line_numbers)?;
                }
                SsaOp::GetField { .. } | SsaOp::GetStatic { .. } | SsaOp::PutField { .. } | SsaOp::PutStatic { .. } => {
                    emit_field(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers)?;
                }
                SsaOp::ArrayGet { .. } | SsaOp::ArrayPut { .. } | SsaOp::ArrayLength { .. } => {
                    emit_array(f, &mut insns, alloc, v, &mut positions, line_numbers);
                }
                _ => emit_value(f, &mut insns, alloc, v)?,
            }
        }
        match &f.blocks[b].term {
            Terminator::Return(None) => insns.push(0x0e),
            Terminator::Return(Some(v)) => {
                let val = &f.values[*v as usize];
                let op = if val.wide { 0x10 } else { 0x0f };
                insns.push(op | (reg(*v) << 8));
            }
            Terminator::Fall { target } => {
                // Falls through; the layout must place the target next.
                let next = num.layout.get(pos + 1).copied();
                if next != Some(*target) {
                    let off = insns.len();
                    insns.push(0x28);
                    fixups.push((off, *target, true));
                }
            }
            Terminator::Goto { target } => {
                let next = num.layout.get(pos + 1).copied();
                if next != Some(*target) {
                    let off = insns.len();
                    insns.push(0x28);
                    fixups.push((off, *target, true));
                }
            }
            Terminator::If { jvm_op, operands, taken, .. } => {
                let (dexop, two) = crate::bootstrap::cond_branch_dex_op(*jvm_op).unwrap();
                if two {
                    let a = reg(operands[0]);
                    let b2 = reg(operands[1]);
                    insns.push(dexop | ((a & 0xf) << 8) | ((b2 & 0xf) << 12));
                } else {
                    insns.push(dexop | (reg(operands[0]) << 8));
                }
                let off = insns.len();
                insns.push(0);
                fixups.push((off, *taken, false));
            }
        }
    }

    // Resolve branch offsets.
    for (off, target, is_goto) in fixups {
        let tgt = block_unit[target] as i32;
        if is_goto {
            let rel = tgt - off as i32; // goto offset is from the op word itself
            if !(-128..=127).contains(&rel) {
                bail!("ssa dexbuilder: goto offset {rel} needs goto/16 (not yet supported)");
            }
            insns[off] = 0x28 | (((rel as i8) as u8 as u16) << 8);
        } else {
            let rel = tgt - (off as i32 - 1); // if offset is from the op word (off-1)
            insns[off] = rel as i16 as u16;
        }
    }

    let registers_size = alloc.registers_used.max(f.num_arg_registers);
    crate::regalloc::remap_insns(&mut insns, f.num_arg_registers, registers_size);
    let debug_info = crate::bootstrap::build_debug_info(&positions, params);
    Ok(CodeItem {
        registers_size,
        ins_size: f.num_arg_registers,
        outs_size: outs,
        insns,
        fixups: pool_fixups,
        tries: Vec::new(),
        debug_info,
    })
}

/// Emits a method call: `invoke-* {args}` (35c) + a `move-result*` when the call
/// returns a value (the result lands in `reg(v)`). Records the throwing-position
/// for debug info, the method-ref fixup, and the outgoing-register count.
#[allow(clippy::too_many_arguments)]
fn emit_invoke(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    outs: &mut u16,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let (dex_op, method, args, ret, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::Invoke { dex_op, method, args, ret, jvm_pc } => (*dex_op, method, args, ret, *jvm_pc),
        _ => unreachable!(),
    };
    // Expand args into registers; a wide (long/double) arg occupies a pair.
    let mut regs: Vec<u16> = Vec::new();
    for &a in args {
        let r = reg(a);
        regs.push(r);
        if f.values[a as usize].wide {
            regs.push(r + 1);
        }
    }
    if regs.len() > 5 || regs.iter().any(|&r| r > 15) {
        bail!("ssa dexbuilder: invoke needs range form / >16 registers (not yet supported)");
    }
    // A throwing instruction records a debug position at its dex address.
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    let argn = regs.len() as u16;
    let g = if regs.len() == 5 { regs[4] } else { 0 };
    insns.push(dex_op | (((argn << 4) | g) << 8));
    let method_unit = insns.len();
    insns.push(0); // method-ref placeholder, patched via the fixup
    let mut nib: u16 = 0;
    for (k, &r) in regs.iter().take(4).enumerate() {
        nib |= r << (4 * k);
    }
    insns.push(nib);
    pool_fixups.push(Fixup { unit: method_unit, item: ItemRef::Method(method.clone()), wide: false });
    *outs = (*outs).max(argn);
    if let Some(rk) = ret {
        let dest = reg(v);
        let mv: u16 = if rk.wide { 0x0b } else if rk.is_ref { 0x0c } else { 0x0a };
        insns.push(mv | (dest << 8));
    }
    Ok(())
}

/// Emits a field access: `iget/sget` (21c/22c, result in `reg(v)`) or
/// `iput/sput` (the value is the source). All field accesses are throwing, so a
/// debug position is recorded; the field-ref word is a fixup placeholder.
fn emit_field(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let (dex_op, field, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::GetField { dex_op, field, jvm_pc, .. }
        | SsaOp::GetStatic { dex_op, field, jvm_pc, .. }
        | SsaOp::PutField { dex_op, field, jvm_pc, .. }
        | SsaOp::PutStatic { dex_op, field, jvm_pc, .. } => (*dex_op, field.clone(), *jvm_pc),
        _ => unreachable!(),
    };
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    match &f.values[v as usize].op {
        // 21c: sget/sput AA = dest/value.
        SsaOp::GetStatic { .. } => insns.push(dex_op | (reg(v) << 8)),
        SsaOp::PutStatic { value, .. } => insns.push(dex_op | (reg(*value) << 8)),
        // 22c: iget/iput A = dest/value (low nibble), B = object (high nibble).
        SsaOp::GetField { obj, .. } => {
            insns.push(dex_op | ((reg(v) & 0xf) << 8) | ((reg(*obj) & 0xf) << 12));
        }
        SsaOp::PutField { obj, value, .. } => {
            insns.push(dex_op | ((reg(*value) & 0xf) << 8) | ((reg(*obj) & 0xf) << 12));
        }
        _ => unreachable!(),
    }
    let unit = insns.len();
    insns.push(0); // field-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::Field(field), wide: false });
    Ok(())
}

/// Emits an array access: `aget*/aput*` (23x: AA=dest/value, BB=array, CC=index)
/// or `array-length` (12x). All are throwing → a debug position is recorded.
fn emit_array(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) {
    let reg = |x: ValId| alloc.reg[x as usize];
    let jvm_pc = match &f.values[v as usize].op {
        SsaOp::ArrayGet { jvm_pc, .. } | SsaOp::ArrayPut { jvm_pc, .. } | SsaOp::ArrayLength { jvm_pc, .. } => *jvm_pc,
        _ => unreachable!(),
    };
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    match &f.values[v as usize].op {
        SsaOp::ArrayGet { dex_op, array, index, .. } => {
            insns.push(dex_op | (reg(v) << 8));
            insns.push((reg(*array) & 0xff) | ((reg(*index) & 0xff) << 8));
        }
        SsaOp::ArrayPut { dex_op, array, index, value, .. } => {
            insns.push(dex_op | (reg(*value) << 8));
            insns.push((reg(*array) & 0xff) | ((reg(*index) & 0xff) << 8));
        }
        // array-length (0x21, 12x): A = dest (low nibble), B = array (high nibble).
        SsaOp::ArrayLength { array, .. } => {
            insns.push(0x21 | ((reg(v) & 0xf) << 8) | ((reg(*array) & 0xf) << 12));
        }
        _ => unreachable!(),
    }
}

/// Emits the instruction defining `v` (the result lands in `reg(v)`).
fn emit_value(f: &SsaFn, insns: &mut Vec<u16>, alloc: &Allocation, v: ValId) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let dest = reg(v);
    match &f.values[v as usize].op {
        SsaOp::ConstInt(c) => emit_const_int(insns, dest, *c),
        SsaOp::ConstLong(c) => emit_const_long(insns, dest, *c),
        SsaOp::Unop { jvm_op, a } => {
            let dop = match jvm_op {
                0x74 => 0x7b, 0x75 => 0x7d, 0x76 => 0x7f, 0x77 => 0x80,
                other => bail!("ssa dexbuilder: unop {other:#x} unsupported"),
            };
            insns.push(dop | ((dest & 0xf) << 8) | ((reg(*a) & 0xf) << 12));
        }
        SsaOp::Cmp { jvm_op, a, b } => {
            let (dop, _) = crate::bootstrap::cmp_op(*jvm_op);
            insns.push(dop | (dest << 8));
            insns.push((reg(*a) & 0xff) | ((reg(*b) & 0xff) << 8));
        }
        SsaOp::Binop { jvm_op, a, b } => emit_binop(f, insns, alloc, dest, *jvm_op, *a, *b)?,
        // Invokes/field-accesses are emitted by `emit_invoke`/`emit_field` (they
        // carry extra state); the rest define no emittable instruction on their own.
        SsaOp::Phi { .. }
        | SsaOp::Argument { .. }
        | SsaOp::ConstString(_)
        | SsaOp::Invoke { .. }
        | SsaOp::GetField { .. }
        | SsaOp::GetStatic { .. }
        | SsaOp::PutField { .. }
        | SsaOp::PutStatic { .. }
        | SsaOp::ArrayGet { .. }
        | SsaOp::ArrayPut { .. }
        | SsaOp::ArrayLength { .. } => {
            bail!("ssa dexbuilder: value {v} has no emittable instruction")
        }
    }
    Ok(())
}

fn emit_binop(f: &SsaFn, insns: &mut Vec<u16>, alloc: &Allocation, dest: u16, jvm_op: u8, a: ValId, b: ValId) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    // Lit-fold when the right operand is a rematerialized small constant.
    if is_rematerialized(f, b) {
        if let SsaOp::ConstInt(c) = f.values[b as usize].op {
            if let Some((op8, op16)) = crate::bootstrap::lit_ops(jvm_op) {
                if (-128..=127).contains(&c) {
                    insns.push(op8 | (dest << 8));
                    insns.push((reg(a) & 0xff) | (((c as u16) & 0xff) << 8));
                    return Ok(());
                } else {
                    insns.push(op16 | ((dest as u16) << 8) | ((reg(a) as u16) << 12));
                    insns.push(c as u16);
                    return Ok(());
                }
            }
        }
    }
    let (ra, rb) = (reg(a), reg(b));
    let mul_bug = is_mul_bug_min_api() && crate::bootstrap::is_mul_op(jvm_op);
    if let Some(op2) = crate::bootstrap::binop_2addr_op(jvm_op) {
        if !mul_bug && dest == ra {
            insns.push(op2 | ((dest as u16) << 8) | ((rb as u16) << 12));
            return Ok(());
        }
        if !mul_bug && crate::bootstrap::is_commutative(jvm_op) && dest == rb {
            insns.push(op2 | ((dest as u16) << 8) | ((ra as u16) << 12));
            return Ok(());
        }
    }
    // 3-address form keeps the operands in source order (d8 does NOT canonicalize
    // commutative sources: `p*i` with p=v1,i=v0 stays `mul-int v1, v1, v0`).
    let op3 = crate::bootstrap::binop_3addr_op(jvm_op)?;
    insns.push(op3 | (dest << 8));
    insns.push((ra & 0xff) | ((rb & 0xff) << 8));
    Ok(())
}

/// The IR path currently targets min-api 1, where the mul-2addr bug applies.
fn is_mul_bug_min_api() -> bool {
    true
}

fn emit_const_int(insns: &mut Vec<u16>, reg: u16, c: i32) {
    if (-8..=7).contains(&c) {
        insns.push(0x12 | (((c as u16 & 0xf) << 4 | reg) << 8));
    } else if (-32768..=32767).contains(&c) {
        insns.push(0x13 | (reg << 8));
        insns.push(c as u16);
    } else if c & 0xffff == 0 {
        insns.push(0x15 | (reg << 8));
        insns.push((c >> 16) as u16);
    } else {
        insns.push(0x14 | (reg << 8));
        insns.push(c as u16);
        insns.push((c >> 16) as u16);
    }
}

fn emit_const_long(insns: &mut Vec<u16>, reg: u16, c: i64) {
    if (-32768..=32767).contains(&c) {
        insns.push(0x16 | (reg << 8));
        insns.push(c as u16);
    } else if (i32::MIN as i64..=i32::MAX as i64).contains(&c) {
        insns.push(0x17 | (reg << 8));
        insns.push(c as u16);
        insns.push((c >> 16) as u16);
    } else if c & 0xffff_ffff_ffff == 0 {
        insns.push(0x19 | (reg << 8));
        insns.push((c >> 48) as u16);
    } else {
        insns.push(0x18 | (reg << 8));
        for k in 0..4 {
            insns.push((c >> (16 * k)) as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn count_loop_phi() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures/Loop.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == "count").unwrap();
        let bc = &m.code.as_ref().unwrap().bytecode;
        let cfg = Cfg::build(bc).unwrap();
        let idom = dominators(&cfg);
        let df = dominance_frontiers(&cfg, &idom);
        let sites = def_sites(&cfg, bc);
        let phis = phi_blocks(&df, &sites, &[0]);
        eprintln!(
            "count: blocks={} idom={:?} df={:?} sites={:?} phis={:?}",
            cfg.len(),
            idom,
            df,
            sites,
            phis
        );
        // `c` (slot 1) is assigned in two blocks → a φ at the loop header.
        assert!(
            phis.get(&1).is_some_and(|s| !s.is_empty()),
            "expected a φ for c (slot 1), got {phis:?}"
        );
    }

    fn build(method: &str, params: &[&str]) -> SsaFn {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures/Loop.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == method).unwrap();
        let bc = &m.code.as_ref().unwrap().bytecode;
        let ps: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        build_ssa(&cf, bc, &ps, false).unwrap()
    }

    /// Runs the full SSA dexer on a method of a fixture class, returning the code.
    fn ssa_code(fixture: &str, method: &str, params: &[&str]) -> CodeItem {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures")
            .join(fixture);
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == method).unwrap();
        let code = m.code.as_ref().unwrap();
        let ps: Vec<String> = params.iter().map(|s| s.to_string()).collect();
        dex_method_ssa(&cf, &code.bytecode, &ps, false, &code.line_numbers).unwrap()
    }

    #[test]
    fn count_ssa_structure() {
        let f = build("count", &["I"]);
        let phi = f
            .values
            .iter()
            .find(|v| matches!(v.op, SsaOp::Phi { .. }))
            .expect("a φ for the loop variable");
        if let SsaOp::Phi { operands, slot } = &phi.op {
            eprintln!("count φ slot={slot} operands={operands:?}, {} values", f.values.len());
            // Loop header has two preds (entry + back-edge) → two φ operands.
            assert_eq!(operands.len(), 2, "loop φ should have one operand per pred");
        }
    }

    #[test]
    fn count_intervals() {
        let f = build("count", &["I"]);
        let num = number(&f);
        let ivs = live_intervals(&f, &num);
        eprintln!("count block_span={:?}", num.block_span);
        for iv in &ivs {
            eprintln!("  v{} [{},{}) {:?}", iv.value, iv.start, iv.end, f.values[iv.value as usize].op);
        }
        // The loop φ for `c` is live across the loop body, so its interval must
        // extend past its own definition point (a single number).
        let phi = f.values.iter().find(|v| matches!(v.op, SsaOp::Phi { .. })).unwrap();
        let iv = ivs.iter().find(|i| i.value == phi.id).expect("interval for loop φ");
        assert!(iv.end > iv.start, "loop φ interval should span the loop: {iv:?}");
    }

    #[test]
    fn count_allocation() {
        let f = build("count", &["I"]);
        let num = number(&f);
        let ivs = live_intervals(&f, &num);
        let alloc = allocate(&f, &num, &ivs);
        // Identify values by op.
        let n = f.values.iter().find(|v| matches!(v.op, SsaOp::Argument { .. })).unwrap().id;
        let phi = f.values.iter().find(|v| matches!(v.op, SsaOp::Phi { .. })).unwrap().id;
        let init = f.values.iter().find(|v| matches!(v.op, SsaOp::ConstInt(0))).unwrap().id;
        let inc = f.values.iter().find(|v| matches!(v.op, SsaOp::ConstInt(1))).unwrap().id;
        let add = f.values.iter().find(|v| matches!(v.op, SsaOp::Binop { .. })).unwrap().id;
        eprintln!(
            "count alloc: n=v{n}->{} phi=v{phi}->{} init=v{init}->{} inc=v{inc}->{} add=v{add}->{} regs={}",
            alloc.reg[n as usize], alloc.reg[phi as usize], alloc.reg[init as usize],
            alloc.reg[inc as usize], alloc.reg[add as usize], alloc.registers_used,
        );
        // The loop variable c (φ + initial + back-edge) is coalesced to one
        // register, distinct from n; the iinc constant is rematerialized.
        assert_eq!(alloc.registers_used, 2);
        assert_eq!(alloc.reg[n as usize], 0, "n is pre-colored to allocated 0");
        let c = alloc.reg[phi as usize];
        assert_eq!(alloc.reg[init as usize], c, "initial c coalesced with φ");
        assert_eq!(alloc.reg[add as usize], c, "back-edge c+1 coalesced with φ");
        assert_ne!(c, 0, "c distinct from n in allocated space");
        assert_eq!(alloc.reg[inc as usize], NO_REG, "iinc constant rematerialized");
        // After the args-high remap: c→v0 (low), n→v1 (high) — exactly d8.
        assert_eq!(crate::regalloc::remap_register(alloc.reg[phi as usize], 1, 2), 0);
        assert_eq!(crate::regalloc::remap_register(alloc.reg[n as usize], 1, 2), 1);
    }

    #[test]
    fn count_dex_byte_identical() {
        let code = ssa_code("Loop.class", "count", &["I"]);
        // d8: const/4 v0,#0; if-ge v0,v1,+5; add-int/lit8 v0,v0,#1; goto -4; return v0
        // (little-endian code units).
        let expected: Vec<u16> = vec![0x0012, 0x1035, 0x0005, 0x00d8, 0x0100, 0xfc28, 0x000f];
        eprintln!(
            "count IR-path insns: {:04x?} (regs={})",
            code.insns, code.registers_size
        );
        assert_eq!(code.registers_size, 2);
        assert_eq!(code.insns, expected, "count loop must be byte-identical to d8");
    }

    #[test]
    fn sumtwice_static_call_in_loop() {
        // `for(i=0;i<n;i++) s += twice(i)` — a static call inside a loop:
        // invoke-static {v0} + move-result v2 + add-int/2addr; the call is a
        // throwing instruction → one debug position. The method-ref word is a
        // fixup placeholder (0) here, patched to its pool index by the writer.
        let code = ssa_code("Calls.class", "sumTwice", &["I"]);
        let expected: Vec<u16> = vec![
            0x0012, 0x0112, 0x3035, 0x000a, // const i,s; if-ge i,n,+10
            0x1071, 0x0000, 0x0000, // invoke-static {v0}, <placeholder>
            0x020a, // move-result v2
            0x21b0, // add-int/2addr v1, v2
            0x00d8, 0x0100, // add-int/lit8 v0, v0, #1
            0xf728, // goto -9
            0x010f, // return v1
        ];
        eprintln!("sumTwice produced: {:04x?} (regs={} outs={})", code.insns, code.registers_size, code.outs_size);
        assert_eq!(code.insns, expected, "static call in loop must match d8 (modulo pool idx)");
        assert_eq!(code.registers_size, 4);
        assert_eq!(code.outs_size, 1);
        assert_eq!(code.fixups.len(), 1, "one method-ref fixup");
        let dbg = code.debug_info.expect("invoke is throwing → a debug position");
        assert_eq!(dbg.line_start, 3, "position line for the call site");
    }

    #[test]
    fn sumto_dex_byte_identical() {
        let code = ssa_code("Loop.class", "sumTo", &["I"]);
        // d8: const/4 v0,#0; const/4 v1,#0; if-ge v0,v2,+6; add-int/2addr v1,v0;
        //     add-int/lit8 v0,v0,#1; goto -5; return v1
        // (i is the counter → v0/low register; s is the accumulator → v1; n → v2).
        let expected: Vec<u16> =
            vec![0x0012, 0x0112, 0x2035, 0x0006, 0x01b0, 0x00d8, 0x0100, 0xfb28, 0x010f];
        eprintln!("sumTo produced: {:04x?} (regs={})", code.insns, code.registers_size);
        assert_eq!(code.registers_size, 3);
        assert_eq!(code.insns, expected, "two-loop-variable loop must be byte-identical to d8");
    }

    fn diag(name: &str, expected: &[u16], regs: u16) -> bool {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures/Loops2.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == name).unwrap();
        let c = m.code.as_ref().unwrap();
        let code = match dex_method_ssa(&cf, &c.bytecode, &["I".to_string()], false, &c.line_numbers) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{name} BAILS: {e}");
                return false;
            }
        };
        eprintln!("{name} produced: {:04x?} (regs={})", code.insns, code.registers_size);
        eprintln!("{name} expected: {expected:04x?} (regs={regs})");
        let ok = code.insns == expected && code.registers_size == regs;
        eprintln!("{name}: {}", if ok { "MATCH" } else { "DIVERGES" });
        ok
    }

    #[test]
    fn down_dex_byte_identical() {
        // `for(i=n;i>0;i--) s+=i` — counter i coalesces with the arg n (no init
        // const), if-lez condition, s+=i (2addr), i-- (add-int/lit8 -1).
        let expected = [0x0012, 0x013d, 0x0006, 0x10b0, 0x01d8, 0xff01, 0xfb28, 0x000f];
        assert!(diag("down", &expected, 2), "down must match d8");
    }

    #[test]
    fn fact_dex_byte_identical() {
        // `for(i=1;i<=n;i++) p*=i` — if-gt condition; the mul-int bug forces the
        // 3-address form, whose sources keep source order (`mul-int v1, v1, v0`).
        let expected =
            [0x1012, 0x1112, 0x2036, 0x0007, 0x0192, 0x0001, 0x00d8, 0x0100, 0xfa28, 0x010f];
        assert!(diag("fact", &expected, 3), "fact must match d8");
    }

    #[test]
    fn grid_dex_diff() {
        // Nested loop, three live loop vars (i, j, accumulator t) + a temp. 6
        // registers. Diagnostic only — the SSA path currently BAILS (the nested
        // loop needs d8's φ-move insertion + partial coalescing, which leaves a
        // dead `const/4 v0`; we fully coalesce instead). Falls back to the CFG
        // path in the real dexer. Marks the nested-loop frontier.
        let expected = [
            0x0012, 0x0112, 0x0212, 0x5135, 0x000e, 0x0312, 0x5335, 0x0008, 0x0492, 0x0301,
            0x42b0, 0x03d8, 0x0103, 0xf928, 0x01d8, 0x0101, 0xf328, 0x020f,
        ];
        let _ = diag("grid", &expected, 6);
    }

    #[test]
    fn sumto_ssa_structure() {
        let f = build("sumTo", &["I"]);
        let phi_count = f.values.iter().filter(|v| matches!(v.op, SsaOp::Phi { .. })).count();
        eprintln!("sumTo: {} φ-nodes, {} values, {} blocks", phi_count, f.values.len(), f.blocks.len());
        // Two loop-carried locals (i and s) → two φ-nodes at the header.
        assert_eq!(phi_count, 2, "sumTo should have φ-nodes for i and s");
        for v in &f.values {
            if let SsaOp::Phi { operands, .. } = &v.op {
                assert_eq!(operands.len(), 2, "each φ has one operand per pred");
            }
        }
    }
}

/// Blocks needing a φ for each local slot (iterated dominance frontier of its
/// def-sites). Argument slots also count as defined at the entry block.
pub(crate) fn phi_blocks(
    df: &[BTreeSet<usize>],
    def_sites: &BTreeMap<u16, BTreeSet<usize>>,
    arg_slots: &[u16],
) -> BTreeMap<u16, BTreeSet<usize>> {
    let mut result: BTreeMap<u16, BTreeSet<usize>> = BTreeMap::new();
    for (&slot, sites) in def_sites {
        let mut work: Vec<usize> = sites.iter().copied().collect();
        if arg_slots.contains(&slot) {
            work.push(0);
        }
        let mut has_phi: BTreeSet<usize> = BTreeSet::new();
        let mut in_work: BTreeSet<usize> = work.iter().copied().collect();
        while let Some(x) = work.pop() {
            for &y in &df[x] {
                if has_phi.insert(y) {
                    result.entry(slot).or_default().insert(y);
                    if in_work.insert(y) {
                        work.push(y);
                    }
                }
            }
        }
    }
    result
}
