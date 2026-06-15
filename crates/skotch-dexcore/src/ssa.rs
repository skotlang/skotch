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

use crate::bootstrap::{split_blocks_with, Block};
use anyhow::{bail, Result};
use skotch_classfile::model::{ClassFile, ExceptionEntry};
use skotch_dex::model::{CatchHandler, FieldRef, Fixup, ItemRef, MethodRef, ProtoRef, TryItem};
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
    /// A string constant (`const-string dest, string@`). Throwing in d8's model,
    /// so it records a debug position (`jvm_pc` locates the source line).
    ConstString { value: String, jvm_pc: u32 },
    /// `const-class dest, type@` (21c) — the `X.class` literal. Throwing (class init /
    /// NoClassDefFound). Result is a `Ljava/lang/Class;` ref.
    ConstClass { type_desc: String, jvm_pc: u32 },
    /// `jvm_pc` locates the source line for div/rem (which are throwing).
    Binop { jvm_op: u8, a: ValId, b: ValId, jvm_pc: u32 },
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
    /// `monitor-enter vAA` (0x1d) / `monitor-exit vAA` (0x1e), 11x — the `synchronized`
    /// lock acquire/release. A statement (no result). Throwing (NPE on a null monitor;
    /// monitor-exit can also throw IllegalMonitorStateException). `enter` selects the op.
    Monitor { enter: bool, obj: ValId, jvm_pc: u32 },
    /// Array element read (`aget* dest, array, index`, 23x). Throwing (NPE/AIOOBE).
    ArrayGet { dex_op: u16, array: ValId, index: ValId, jvm_pc: u32 },
    /// Array element write (`aput* value, array, index`, 23x). A statement.
    ArrayPut { dex_op: u16, array: ValId, index: ValId, value: ValId, jvm_pc: u32 },
    /// Array length (`array-length dest, array`, 12x). Throwing (NPE).
    ArrayLength { array: ValId, jvm_pc: u32 },
    /// `new-instance dest, type@` (21c). Throwing (OOM/class-init). Result is a ref.
    NewInstance { type_desc: String, jvm_pc: u32 },
    /// `new-array dest, size, type@` (22c). Throwing (NegativeArraySize). Ref result.
    NewArray { type_desc: String, size: ValId, jvm_pc: u32 },
    /// The caught exception at an exception-handler's entry (`move-exception dest`,
    /// 11x). A ref result; emitted only if the caught value is actually used.
    CaughtException,
    /// `check-cast vAA, type@` (21c). Throwing (ClassCastException). In-place in DEX
    /// (operates on one register); SSA models a result value that aliases `obj`, so
    /// emission moves `obj` into the result register first if they didn't coalesce.
    /// Result is a ref.
    CheckCast { obj: ValId, type_desc: String, jvm_pc: u32 },
    /// `instance-of vA, vB, type@` (22c). Non-throwing (null → false). Result is an
    /// int (boolean 0/1).
    InstanceOf { obj: ValId, type_desc: String, jvm_pc: u32 },
}

/// Whether a JVM opcode can throw (for placing exception edges from a try region
/// to its handler). Conservative over the subset the SSA path emits.
fn is_throwing_op(op: u8) -> bool {
    matches!(op,
        0xb6..=0xb9            // invoke*
        | 0xb2..=0xb5          // get/put field/static
        | 0x2e..=0x35          // *aload
        | 0x4f..=0x56          // *astore
        | 0xbe                 // arraylength
        | 0x6c | 0x6d | 0x70 | 0x71  // i/l div/rem
        | 0xbb..=0xbd          // new / newarray / anewarray
        | 0xbf | 0xc0          // athrow / checkcast
        | 0xc2 | 0xc3          // monitorenter / monitorexit
        | 0x12 | 0x13          // ldc / ldc_w (const-string)
    )
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
    /// Holds an object reference (selects `move-object` for a φ-move / `return-object`).
    /// Inferred in a post-pass after construction.
    pub(crate) is_ref: bool,
    pub(crate) block: usize,
}

/// A block terminator (control flow leaving the block).
#[derive(Clone, Debug)]
pub(crate) enum Terminator {
    /// Conditional branch on `cond` operands (1 or 2) to `taken`, else fall to
    /// `fallthrough`.
    If { jvm_op: u8, operands: Vec<ValId>, taken: usize, fallthrough: usize },
    Goto { target: usize },
    /// Returns `value` (None for void); `op` is the DEX return opcode
    /// (0x0e return-void, 0x0f return, 0x10 return-wide, 0x11 return-object).
    Return { value: Option<ValId>, op: u16 },
    /// Falls through to the single successor with no explicit branch.
    Fall { target: usize },
    /// `throw vAA` (0x27, 11x). Ends the block with no NORMAL successor; the
    /// exceptional edge to a covering handler is carried by `exc_succ`/the handler-φ
    /// snapshot (athrow is in `is_throwing_op`), not by `succ`. `jvm_pc` locates the
    /// athrow so its emitted span joins the try_item range (it IS a guarded throwing
    /// instruction — the one that actually raises, so the handler must cover it).
    Throw { value: ValId, jvm_pc: u32 },
    /// `tableswitch`/`lookupswitch` on `value`. Lowered (functional-correctness, not
    /// d8's packed/sparse-switch payload) to a `const tmp,k; if-eq value,tmp,case`
    /// chain + `goto default` in the emit phase. `cases` is `(key, target-block)`;
    /// `default` is the fall-through block.
    Switch { value: ValId, default: usize, cases: Vec<(i32, usize)> },
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
    /// Exception-handler blocks reachable from a throwing instruction in this block
    /// (for liveness — a handler's live-in flows back as this block's live-out).
    pub(crate) exc_succ: Vec<usize>,
}

/// A resolved try region for DEX `try_item` emission: a JVM pc range whose throwing
/// instructions are guarded by `handler_block` catching `catch_type` (None = any).
#[derive(Clone, Debug)]
pub(crate) struct ExcRegion {
    pub(crate) start_pc: usize,
    pub(crate) end_pc: usize,
    pub(crate) handler_block: usize,
    pub(crate) catch_type: Option<String>,
}

pub(crate) struct SsaFn {
    pub(crate) values: Vec<SsaValue>,
    pub(crate) blocks: Vec<SsaBlock>,
    pub(crate) num_arg_registers: u16,
    /// Try regions (for try_item emission); empty when the method has no try/catch.
    pub(crate) exc_regions: Vec<ExcRegion>,
    /// Per block: the caught-exception value at a handler's entry (`None` otherwise).
    pub(crate) caught: Vec<Option<ValId>>,
}

/// The control-flow graph: blocks with successor AND predecessor edges. Exception
/// edges (a try-region block → its handler) are reflected in `preds` (so dominance
/// and φ-placement treat the handler as dominated by the try) and listed in
/// `exc_edges` (so rename can give the handler the throw-point state, not the
/// block-exit state).
pub(crate) struct Cfg {
    pub(crate) blocks: Vec<Block>,
    pub(crate) preds: Vec<Vec<usize>>,
    /// Reverse-postorder (entry first); the order dominance iterates in.
    pub(crate) rpo: Vec<usize>,
    /// (try-region block, handler block) exception edges.
    pub(crate) exc_edges: Vec<(usize, usize)>,
}

impl Cfg {
    pub(crate) fn build(bc: &[u8], exceptions: &[ExceptionEntry]) -> Result<Cfg> {
        let handler_pcs: Vec<usize> = exceptions.iter().map(|e| e.handler_pc as usize).collect();
        let blocks = split_blocks_with(bc, &handler_pcs)?;
        let n = blocks.len();
        let mut preds = vec![Vec::new(); n];
        for (b, blk) in blocks.iter().enumerate() {
            for &s in &blk.succ {
                preds[s].push(b);
            }
        }
        // Exception edges: a block overlapping a try region [start,end) and holding
        // a throwing instruction there gets an edge to that region's handler.
        let leader_block = |pc: usize| blocks.iter().position(|b| b.start == pc);
        let mut exc_edges: Vec<(usize, usize)> = Vec::new();
        for e in exceptions {
            let h = leader_block(e.handler_pc as usize)
                .ok_or_else(|| anyhow::anyhow!("ssa: handler pc {} not a block leader", e.handler_pc))?;
            let (s, t) = (e.start_pc as usize, e.end_pc as usize);
            for (bi, blk) in blocks.iter().enumerate() {
                if blk.start >= t || blk.end <= s {
                    continue; // no overlap with the try region
                }
                let mut pc = blk.start.max(s);
                let mut throws = false;
                while pc < blk.end.min(t) {
                    if is_throwing_op(bc[pc]) {
                        throws = true;
                        break;
                    }
                    pc += crate::bootstrap::jvm_step_len(bc, pc);
                }
                if throws {
                    exc_edges.push((bi, h));
                    if !preds[h].contains(&bi) {
                        preds[h].push(bi);
                    }
                }
            }
        }
        let rpo = reverse_postorder(&blocks, &exc_edges);
        Ok(Cfg { blocks, preds, rpo, exc_edges })
    }

    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }
}

/// Synthesize an empty pre-header as the new entry (block 0), shifting every existing
/// block index up by one. Used when the original entry block is a loop header (a back-edge
/// targets pc 0, e.g. `while(b!=0){…}` as the whole method body): the header then has both
/// the pre-header (the function-entry edge) AND the back-edge as predecessors, so
/// dominance-frontier φ-placement gives the loop variables their φs, whose entry operand is
/// the incoming argument value. The pre-header carries no bytecode (start==end==0); rename
/// gives it a `Fall` to block 1 and seeds the argument values there before the loop. Only
/// sound with no exception regions (handler/try-block indices would also need shifting) —
/// the sole caller guards on `exceptions.is_empty()`.
fn insert_entry_preheader(cfg: Cfg) -> Cfg {
    let mut blocks: Vec<Block> = Vec::with_capacity(cfg.blocks.len() + 1);
    // The pre-header: no bytecode, falls through to the former entry (now block 1).
    blocks.push(Block { start: 0, end: 0, succ: vec![1] });
    for mut blk in cfg.blocks {
        blk.succ = blk.succ.iter().map(|&s| s + 1).collect();
        blocks.push(blk);
    }
    let n = blocks.len();
    let mut preds = vec![Vec::new(); n];
    for (b, blk) in blocks.iter().enumerate() {
        for &s in &blk.succ {
            preds[s].push(b);
        }
    }
    // Exception edges shift with their blocks (empty here, but keep the graph consistent).
    let exc_edges: Vec<(usize, usize)> = cfg.exc_edges.iter().map(|&(a, h)| (a + 1, h + 1)).collect();
    for &(from, h) in &exc_edges {
        if !preds[h].contains(&from) {
            preds[h].push(from);
        }
    }
    let rpo = reverse_postorder(&blocks, &exc_edges);
    Cfg { blocks, preds, rpo, exc_edges }
}

/// Reverse postorder from block 0 over normal + exception successors.
fn reverse_postorder(blocks: &[Block], exc_edges: &[(usize, usize)]) -> Vec<usize> {
    let n = blocks.len();
    let mut succ: Vec<Vec<usize>> = blocks.iter().map(|b| b.succ.clone()).collect();
    for &(from, h) in exc_edges {
        if !succ[from].contains(&h) {
            succ[from].push(h);
        }
    }
    let mut visited = vec![false; n];
    let mut post = Vec::with_capacity(n);
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    visited[0] = true;
    while let Some(&(b, _)) = stack.last() {
        let i = stack.last().unwrap().1;
        if i < succ[b].len() {
            stack.last_mut().unwrap().1 += 1;
            let s = succ[b][i];
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
                // A wide (long/double) value is ONE SSA variable named by its low
                // slot; the high half (slot+1) is never read independently in valid
                // bytecode, so adding it as a def-site would place a φ whose operands
                // are undefined (a spurious bail). Track only the low slot.
                sites.entry(slot as u16).or_default().insert(bi);
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

/// Whether `a` dominates `b` (walking `b`'s immediate-dominator chain to `a`).
fn dominates(idom: &[usize], a: usize, b: usize) -> bool {
    let mut x = b;
    loop {
        if x == a {
            return true;
        }
        if x == 0 || idom[x] == usize::MAX || idom[x] == x {
            return false;
        }
        x = idom[x];
    }
}

/// The local slot a load opcode reads at `pc`, or `None` if not a load.
fn load_slot_at(bc: &[u8], pc: usize) -> Option<u16> {
    Some(match bc[pc] {
        0x15..=0x19 => bc[pc + 1] as u16,         // i/l/f/d/aload <idx>
        0x1a..=0x1d => (bc[pc] - 0x1a) as u16,     // iload_n
        0x1e..=0x21 => (bc[pc] - 0x1e) as u16,     // lload_n
        0x22..=0x25 => (bc[pc] - 0x22) as u16,     // fload_n
        0x26..=0x29 => (bc[pc] - 0x26) as u16,     // dload_n
        0x2a..=0x2d => (bc[pc] - 0x2a) as u16,     // aload_n
        _ => return None,
    })
}

/// Collects the local slots assigned (`*store` / `iinc`) within the JVM pc range
/// `[start, end)` — the locals a try region can redefine before throwing.
fn collect_def_slots(bc: &[u8], start: usize, end: usize, out: &mut BTreeSet<u16>) {
    let mut pc = start;
    while pc < end {
        if let Some((s, len)) = crate::bootstrap::store_slot(bc, pc) {
            out.insert(s as u16);
            pc += len;
        } else if bc[pc] == 0x84 {
            out.insert(bc[pc + 1] as u16); // iinc
            pc += 3;
        } else {
            pc += crate::bootstrap::jvm_step_len(bc, pc);
        }
    }
}

/// Live-in sets of LOCAL slots per block (backward dataflow). Used for PRUNED SSA:
/// a φ is placed for slot `s` at block `B` only when `s` is live-in at `B`, so
/// loop-body-only locals don't get spurious loop-header φs with undefined pre-loop
/// entries (the cause of "use of undefined local slot" bails).
fn slot_liveness(cfg: &Cfg, bc: &[u8]) -> Vec<BTreeSet<u16>> {
    let n = cfg.len();
    let mut use_: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    let mut def_: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    for (bi, blk) in cfg.blocks.iter().enumerate() {
        let mut defined: BTreeSet<u16> = BTreeSet::new();
        let mut pc = blk.start;
        while pc < blk.end {
            if let Some(s) = load_slot_at(bc, pc) {
                if !defined.contains(&s) {
                    use_[bi].insert(s);
                }
                pc += crate::bootstrap::jvm_step_len(bc, pc);
            } else if let Some((s, len)) = crate::bootstrap::store_slot(bc, pc) {
                def_[bi].insert(s as u16);
                defined.insert(s as u16);
                pc += len;
            } else if bc[pc] == 0x84 {
                let s = bc[pc + 1] as u16; // iinc: a use AND a def
                if !defined.contains(&s) {
                    use_[bi].insert(s);
                }
                def_[bi].insert(s);
                defined.insert(s);
                pc += 3;
            } else {
                pc += crate::bootstrap::jvm_step_len(bc, pc);
            }
        }
    }
    let mut live_in: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for b in (0..n).rev() {
            let mut lo: BTreeSet<u16> = BTreeSet::new();
            for &s in &cfg.blocks[b].succ {
                for &v in &live_in[s] {
                    lo.insert(v);
                }
            }
            // A throw in `b` transfers control to its handler with everything live
            // at the handler entry still live — so the handler's live-in is part of
            // `b`'s live-out (keeps try-region locals from being pruned away).
            for &(from, h) in &cfg.exc_edges {
                if from == b {
                    for &v in &live_in[h] {
                        lo.insert(v);
                    }
                }
            }
            let mut li = use_[b].clone();
            for &v in &lo {
                if !def_[b].contains(&v) {
                    li.insert(v);
                }
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
    live_in
}

/// Simulates a block's net effect on the operand stack, tracking the WIDTH (one
/// entry per value, wide=true for long/double) of each entry — mirroring the stack
/// effects in `rename`. Used to compute per-block entry stacks for stack-merge φs.
fn sim_block(bc: &[u8], start: usize, end: usize, cf: &ClassFile, entry: &[bool]) -> Result<Vec<bool>> {
    let mut st: Vec<bool> = entry.to_vec();
    let pop = |st: &mut Vec<bool>| { st.pop(); };
    let mut pc = start;
    while pc < end {
        let op = bc[pc];
        match op {
            0x1a..=0x1d | 0x22..=0x25 | 0x2a..=0x2d => { st.push(false); pc += 1; }   // i/f/a load_n
            0x1e..=0x21 | 0x26..=0x29 => { st.push(true); pc += 1; }                  // l/d load_n
            0x15 | 0x17 | 0x19 => { st.push(false); pc += 2; }                         // iload/fload/aload
            0x16 | 0x18 => { st.push(true); pc += 2; }                                 // lload/dload
            0x02..=0x08 | 0x0b..=0x0d => { st.push(false); pc += 1; }                  // iconst/fconst
            0x10 => { st.push(false); pc += 2; }                                       // bipush
            0x11 => { st.push(false); pc += 3; }                                       // sipush
            0x09 | 0x0a | 0x0e | 0x0f => { st.push(true); pc += 1; }                   // lconst/dconst
            0x12 => { st.push(false); pc += 2; }                                       // ldc
            0x13 => { st.push(false); pc += 3; }                                       // ldc_w
            0x14 => { st.push(true); pc += 3; }                                        // ldc2_w
            0x36..=0x3a => { pop(&mut st); pc += 2; }                                  // store <idx>
            0x3b..=0x4e => { pop(&mut st); pc += 1; }                                  // store_n
            0x84 => { pc += 3; }                                                       // iinc
            0x60..=0x73 | 0x78..=0x83 => { pop(&mut st); pop(&mut st); st.push(binop_result_wide(op)); pc += 1; }
            0x74..=0x77 => { let w = st.pop().unwrap_or(false); st.push(w); pc += 1; } // neg
            0x85..=0x93 => { pop(&mut st); st.push(conv_result_wide(op)); pc += 1; }   // conversions
            0x94..=0x98 => { pop(&mut st); pop(&mut st); st.push(false); pc += 1; }    // cmp
            0x99..=0x9e | 0xc6 | 0xc7 => { pop(&mut st); pc += 3; }                    // if<cond> / ifnull / ifnonnull
            0x9f..=0xa6 => { pop(&mut st); pop(&mut st); pc += 3; }                    // if_icmp<cond> / if_acmp<eq,ne>
            0xa7 => { pc += 3; }                                                       // goto
            0xac..=0xb0 => { pop(&mut st); pc += 1; }                                  // <t>return
            0xb1 => { pc += 1; }                                                       // return-void
            0xb6..=0xb9 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let (_, _, desc) = cf.constant_pool.member_ref(idx)?;
                let (mparams, ret) = crate::bootstrap::parse_descriptor(&desc)?;
                for _ in 0..(mparams.len() + if op == 0xb8 { 0 } else { 1 }) { pop(&mut st); }
                if ret != "V" { st.push(ret == "J" || ret == "D"); }
                pc += if op == 0xb9 { 5 } else { 3 };
            }
            // invokedynamic — string-concat: pops the dynamic args, pushes the String result.
            0xba => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let nt = match cf.constant_pool.get(idx) {
                    skotch_classfile::constant_pool::Constant::InvokeDynamic { name_and_type_index, .. } => *name_and_type_index,
                    _ => return Err(anyhow::anyhow!("ssa stack-sim: indy not InvokeDynamic")),
                };
                let (_, desc) = cf.constant_pool.name_and_type(nt)?;
                let (params, _) = crate::bootstrap::parse_descriptor(desc)?;
                for _ in 0..params.len() { pop(&mut st); }
                st.push(false); // String result
                pc += 5;
            }
            0xb2 => { let (_, _, d) = cf.constant_pool.member_ref(u16::from_be_bytes([bc[pc+1], bc[pc+2]]))?; st.push(d == "J" || d == "D"); pc += 3; }
            0xb3 => { pop(&mut st); pc += 3; }
            0xb4 => { let (_, _, d) = cf.constant_pool.member_ref(u16::from_be_bytes([bc[pc+1], bc[pc+2]]))?; pop(&mut st); st.push(d == "J" || d == "D"); pc += 3; }
            0xb5 => { pop(&mut st); pop(&mut st); pc += 3; }
            0xbe => { pop(&mut st); st.push(false); pc += 1; }                         // arraylength
            0x2e..=0x35 => { pop(&mut st); pop(&mut st); st.push(op == 0x2f || op == 0x31); pc += 1; } // aget
            0x4f..=0x56 => { pop(&mut st); pop(&mut st); pop(&mut st); pc += 1; }      // astore
            0xbb => { st.push(false); pc += 3; }                                       // new
            0xbc => { pop(&mut st); st.push(false); pc += 2; }                         // newarray
            0xbd => { pop(&mut st); st.push(false); pc += 3; }                         // anewarray
            0x59 => { let w = *st.last().unwrap_or(&false); st.push(w); pc += 1; }      // dup
            // swap: `..., v2, v1 → ..., v1, v2` (both category-1).
            0x5f => { let n = st.len(); st.swap(n - 2, n - 1); pc += 1; }
            // dup_x1: `..., v2, v1 → ..., v1, v2, v1` (both category-1).
            0x5a => {
                let v1 = st.pop().unwrap(); let v2 = st.pop().unwrap();
                st.push(v1); st.push(v2); st.push(v1); pc += 1;
            }
            // dup_x2: insert a copy of the cat-1 top below the next one OR two values.
            // Form 2 (the value below is category-2/wide): `w, v1 → v1, w, v1`.
            // Form 1 (two category-1 below): `v3, v2, v1 → v1, v3, v2, v1`.
            0x5b => {
                let v1 = st.pop().unwrap();
                if *st.last().unwrap_or(&false) {
                    let w = st.pop().unwrap();
                    st.push(v1); st.push(w); st.push(v1);
                } else {
                    let v2 = st.pop().unwrap(); let v3 = st.pop().unwrap();
                    st.push(v1); st.push(v3); st.push(v2); st.push(v1);
                }
                pc += 1;
            }
            // dup2_x1: Form 2 (category-2/wide top): `v2, w → w, v2, w`.
            // Form 1 (two category-1 on top): `v3, v2, v1 → v2, v1, v3, v2, v1`.
            0x5d => {
                if *st.last().unwrap_or(&false) {
                    let w = st.pop().unwrap(); let v2 = st.pop().unwrap();
                    st.push(w); st.push(v2); st.push(w);
                } else {
                    let v1 = st.pop().unwrap(); let v2 = st.pop().unwrap(); let v3 = st.pop().unwrap();
                    st.push(v2); st.push(v1); st.push(v3); st.push(v2); st.push(v1);
                }
                pc += 1;
            }
            // dup2: a category-2 (wide) top duplicates it; else duplicates the top two
            // category-1 values (`a[i]++` / `a[i]+=x` duplicate the array+index).
            0x5c => {
                if *st.last().unwrap_or(&false) {
                    st.push(true);
                } else {
                    let n = st.len();
                    let (a, b) = (st[n - 2], st[n - 1]);
                    st.push(a); st.push(b);
                }
                pc += 1;
            }
            0x57 => { pop(&mut st); pc += 1; }                                         // pop
            0x58 => { let w = st.pop().unwrap_or(false); if !w { pop(&mut st); } pc += 1; } // pop2
            0x00 => { pc += 1; }                                                        // nop (d8 drops it)
            0x01 => { st.push(false); pc += 1; }                                        // aconst_null (→ const 0)
            0xaa | 0xab => { pop(&mut st); pc = crate::bootstrap::parse_switch(bc, pc).2; } // switch (pops key)
            0xbf => { pop(&mut st); pc += 1; }                                          // athrow (block ends)
            0xc0 => { pop(&mut st); st.push(false); pc += 3; }                          // checkcast (in-place ref)
            0xc1 => { pop(&mut st); st.push(false); pc += 3; }                          // instanceof (int result)
            0xc2 | 0xc3 => { pop(&mut st); pc += 1; }                                   // monitorenter / monitorexit (pop objectref)
            other => bail!("ssa stack-sim: unsupported opcode {other:#04x}"),
        }
    }
    Ok(st)
}

/// Per-block entry operand-stack (widths). Forward-propagated in RPO; back-edges
/// agree with the forward entry per JVM verification (loop headers enter empty).
fn entry_stacks(cfg: &Cfg, bc: &[u8], cf: &ClassFile) -> Result<Vec<Vec<bool>>> {
    let n = cfg.len();
    let mut entry: Vec<Option<Vec<bool>>> = vec![None; n];
    entry[0] = Some(Vec::new());
    for &b in &cfg.rpo {
        let e = match &entry[b] {
            Some(e) => e.clone(),
            None => continue,
        };
        let exit = sim_block(bc, cfg.blocks[b].start, cfg.blocks[b].end, cf, &e)?;
        for &s in &cfg.blocks[b].succ {
            if entry[s].is_none() {
                entry[s] = Some(exit.clone());
            }
        }
    }
    Ok(entry.into_iter().map(|x| x.unwrap_or_default()).collect())
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
    exceptions: &[ExceptionEntry],
) -> Result<SsaFn> {
    let mut cfg = Cfg::build(bc, exceptions)?;
    // The entry block having a predecessor means a back-edge targets pc 0 — the loop
    // header IS the entry (a `while`/`for` with no pre-header before it, e.g.
    // `while(b!=0){…}` as the whole body). Such a header is a join of the implicit
    // function-entry edge and the back-edge, but the CFG models only the back-edge
    // (1 pred), so dominance-frontier φ-placement (which needs ≥2 preds) skips it and
    // the loop variables get NO φs — they'd never update (a miscompile). We SYNTHESIZE
    // an empty pre-header as the new entry (block 0) so the header gains the entry edge
    // as a second predecessor; the header φ's entry operand is then the incoming
    // (argument) value, filled by rename when it processes the pre-header. The transform
    // shifts every block index up by one — including exception edges (insert_entry_preheader
    // shifts cfg.exc_edges, and exc_regions are rebuilt below from the raw handler PCs against
    // the SHIFTED blocks), so it is sound with try/catch as well.
    if !cfg.preds[0].is_empty() {
        cfg = insert_entry_preheader(cfg);
    }
    let n = cfg.len();
    let idom = dominators(&cfg);
    // (Nested loops were bailed only to stay byte-identical with d8, which leaves an
    // un-DCE'd dead `const` for the undefined-φ-entry; our DCE drops it. That's a
    // SMALLER-but-correct divergence, fine in functional-correctness mode — no bail.)
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
    // Pruned SSA: place a φ for slot `s` at block `B` only when `s` is live-in at
    // `B`. Removes spurious loop-header φs for loop-body-only locals (whose pre-loop
    // entry would be undefined), matching d8.
    let live_in = slot_liveness(&cfg, bc);
    let phis: BTreeMap<u16, BTreeSet<usize>> = phi_blocks(&df, &sites, &arg_slots)
        .into_iter()
        .map(|(slot, blks)| (slot, blks.into_iter().filter(|&bb| live_in[bb].contains(&slot)).collect()))
        .filter(|(_, blks): &(u16, BTreeSet<usize>)| !blks.is_empty())
        .collect();

    let mut values: Vec<SsaValue> = Vec::new();
    let mut blocks: Vec<SsaBlock> = (0..n)
        .map(|b| SsaBlock {
            phis: Vec::new(),
            body: Vec::new(),
            term: Terminator::Return { value: None, op: 0x0e },
            succ: cfg.blocks[b].succ.clone(),
            preds: cfg.preds[b].clone(),
            exc_succ: cfg.exc_edges.iter().filter(|&&(from, _)| from == b).map(|&(_, h)| h).collect(),
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

    // Stack-merge φs: at a merge (≥2 preds) where the operand stack is non-empty on
    // entry (e.g. a ternary's result before its `store`), each stack slot needs a φ
    // — like a local, but for a stack position. They go into `blocks[blk].phis`
    // AFTER the local φs (so numbering/coalescing/allocation/φ-resolution treat them
    // uniformly) and are tracked in `block_stack_phis` for rename's stack threading.
    let estacks = entry_stacks(&cfg, bc, cf)?;
    let mut block_stack_phis: Vec<Vec<ValId>> = vec![Vec::new(); n];
    for blk in 0..n {
        if cfg.preds[blk].len() >= 2 && !estacks[blk].is_empty() {
            for &wide in &estacks[blk] {
                let id = b.new(SsaOp::Phi { slot: u16::MAX, operands: Vec::new() }, wide, blk);
                blocks[blk].phis.push(id);
                block_stack_phis[blk].push(id);
            }
        }
    }

    // Exception handlers (try/catch). For each handler block we create:
    //   • a `CaughtException` value (the exception on the handler's entry stack;
    //     materialized as `move-exception` only if it's actually read), and
    //   • a handler-φ for each local that is BOTH defined within a guarded try
    //     region AND live at the handler entry — to snapshot the version current at
    //     each throw point (which may differ from the try block's exit version).
    // Operands of the handler-φs are filled by `rename` at each throwing instruction.
    // Conservative bail-guards keep us byte-identical-or-bail: typed catches only,
    // no nested/overlapping regions, a single-block try, a handler with exactly one
    // exceptional predecessor and no normal/stack φs.
    let mut exc_regions: Vec<ExcRegion> = Vec::new();
    let mut handler_phis: Vec<Vec<(u16, ValId)>> = vec![Vec::new(); n];
    let mut caught: Vec<Option<ValId>> = vec![None; n];
    if !exceptions.is_empty() {
        for e in exceptions {
            let catch_type = match &e.catch_type {
                Some(c) => Some(skotch_classfile::constant_pool::internal_to_descriptor(c)),
                // catch-all (finally / synchronized exception path): carried as None →
                // the DEX try_item's catch_all_addr at emission. The handler block is
                // modeled like a typed one (CaughtException → move-exception); the rethrow
                // now emits move-exception (the `used` set counts terminator operands).
                None => None,
            };
            let hb = cfg
                .blocks
                .iter()
                .position(|x| x.start == e.handler_pc as usize)
                .ok_or_else(|| anyhow::anyhow!("ssa: handler pc {} not a block leader", e.handler_pc))?;
            exc_regions.push(ExcRegion {
                start_pc: e.start_pc as usize,
                end_pc: e.end_pc as usize,
                handler_block: hb,
                catch_type,
            });
        }
        // Drop SELF-COVERING catch-all regions. A `synchronized(o){…}` block compiles to an
        // EXTRA exception entry `[handler_start, handler_end) → handler` so that if the handler's
        // own monitor-exit throws, the handler re-runs. That throw is unreachable in practice (the
        // monitor IS held when the handler runs after a body exception, so its monitor-exit always
        // succeeds), and modeling a handler that guards ITSELF would need an exceptional self-edge.
        // Dropping this entry leaves the body's `[body, monitor-exit) → handler` region — a normal
        // single try/catch — intact, and the handler still emits its monitor-exit, so the lock is
        // released on every path (functionally exact, never a dropped monitor → no deadlock). The
        // condition `start_pc == handler-block start` matches ONLY a region whose try begins at its
        // own handler; a try-nested-in-a-handler has a DIFFERENT handler block, so it isn't dropped.
        exc_regions
            .retain(|r| !(r.catch_type.is_none() && r.start_pc == cfg.blocks[r.handler_block].start));
        // No partially-overlapping or nested try regions (identical ranges that share
        // a handler are fine — that's one logical region).
        for i in 0..exc_regions.len() {
            for j in 0..exc_regions.len() {
                if i == j {
                    continue;
                }
                let (a, c) = (&exc_regions[i], &exc_regions[j]);
                let overlap = a.start_pc < c.end_pc && c.start_pc < a.end_pc;
                let identical = a.start_pc == c.start_pc && a.end_pc == c.end_pc;
                if overlap && !identical {
                    bail!("ssa: overlapping / nested try regions not yet supported");
                }
            }
        }
        let handler_blocks: BTreeSet<usize> = exc_regions.iter().map(|r| r.handler_block).collect();
        // Handlers whose guarded region has NO throwing instruction (a try/finally — or
        // synchronized — over a non-throwing body): the exceptional handler copy is unreachable.
        // Drop the region (no try_item) and skip its setup; the dead handler block is later
        // excluded from the emit layout (it has no normal pred and now no exceptional edge).
        let mut dead_handlers: BTreeSet<usize> = BTreeSet::new();
        for &hb in &handler_blocks {
            // A handler entered by normal control flow (loop/fallthrough into it), or
            // by more than one try block, would need pred-indexed φ wiring we don't do.
            if (0..n).any(|p| cfg.blocks[p].succ.contains(&hb)) {
                bail!("ssa: handler block is also a normal successor — unsupported");
            }
            // N>1 exceptional predecessors (a try with multiple throwing ops) is now
            // allowed: each throw point snapshots the slot versions into the handler-φs,
            // and a post-allocation check (in dex_method_ssa) BAILS unless all of a
            // handler-φ's operands coalesced into one register — exceptional edges can't
            // carry φ-moves, so they must already agree (or we bail, never miscompile).
            let exc_preds = cfg.exc_edges.iter().filter(|&&(_, h)| h == hb).count();
            if exc_preds == 0 {
                dead_handlers.insert(hb);
                continue;
            }
            if !block_phi_slots[hb].is_empty() || !block_stack_phis[hb].is_empty() {
                bail!("ssa: handler block needs normal/stack φs — unsupported");
            }
            // A handler nested inside another try region (try-in-catch / rethrow paths).
            let h_pc = cfg.blocks[hb].start;
            if exc_regions.iter().any(|r| r.start_pc <= h_pc && h_pc < r.end_pc) {
                bail!("ssa: handler nested inside a try region — unsupported");
            }
            // The caught exception value (created unconditionally; emitted as
            // move-exception only when read).
            caught[hb] = Some(b.new(SsaOp::CaughtException, false, hb));
            // Locals defined within a guarded region targeting this handler.
            let mut slots: BTreeSet<u16> = BTreeSet::new();
            for r in exc_regions.iter().filter(|r| r.handler_block == hb) {
                collect_def_slots(bc, r.start_pc, r.end_pc, &mut slots);
            }
            for slot in slots {
                if !live_in[hb].contains(&slot) {
                    continue; // dead at the handler entry — no snapshot needed
                }
                let id = b.new(SsaOp::Phi { slot, operands: Vec::new() }, false, hb);
                blocks[hb].phis.push(id);
                handler_phis[hb].push((slot, id));
            }
        }
        // Drop the dead (unreachable-handler) regions so no try_item is emitted for them.
        exc_regions.retain(|r| !dead_handlers.contains(&r.handler_block));
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

    let mut exit_stacks: Vec<Option<Vec<ValId>>> = vec![None; n];
    rename(cf, &cfg, bc, &children, &block_phi_slots, &block_stack_phis, &exc_regions, &handler_phis, &caught, &mut exit_stacks, &mut blocks, &mut b, &mut versions, 0)?;

    // φ-nodes inherit their width from their operands (all the same type). They
    // were created before their operands existed, so fix it up now — to a fixpoint
    // since a φ operand can be another (loop-carried) φ.
    loop {
        let mut changed = false;
        for i in 0..values.len() {
            let opnds: Vec<ValId> = match &values[i].op {
                SsaOp::Phi { operands, .. } => operands.clone(),
                _ => continue,
            };
            let wide = opnds.iter().any(|&o| values[o as usize].wide);
            if wide && !values[i].wide {
                values[i].wide = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Infer object-reference-ness per value (selects move-object / return-object for
    // φ-moves). Base cases from the producing op; φs to a fixpoint over operands.
    {
        let arg_is_ref: Vec<bool> = {
            let mut v = Vec::new();
            if instance {
                v.push(true); // `this`
            }
            for p in params {
                v.push(p.starts_with('L') || p.starts_with('['));
            }
            v
        };
        for i in 0..values.len() {
            values[i].is_ref = match &values[i].op {
                SsaOp::Argument { index } => arg_is_ref.get(*index).copied().unwrap_or(false),
                SsaOp::NewInstance { .. } | SsaOp::NewArray { .. } | SsaOp::ConstString { .. } | SsaOp::CaughtException => true,
                SsaOp::CheckCast { .. } | SsaOp::ConstClass { .. } => true, // cast / Class literal are references
                SsaOp::ArrayGet { dex_op, .. } => *dex_op == 0x46, // aget-object
                SsaOp::GetField { field, .. } | SsaOp::GetStatic { field, .. } => {
                    field.type_.starts_with('L') || field.type_.starts_with('[')
                }
                SsaOp::Invoke { ret: Some(rk), .. } => rk.is_ref,
                _ => false,
            };
        }
        loop {
            let mut changed = false;
            for i in 0..values.len() {
                let opnds: Vec<ValId> = match &values[i].op {
                    SsaOp::Phi { operands, .. } => operands.clone(),
                    _ => continue,
                };
                let r = opnds.iter().any(|&o| values[o as usize].is_ref);
                if r && !values[i].is_ref {
                    values[i].is_ref = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    // An unused caught exception (e.g. `catch (E e) { ... }` where `e` is never
    // read) needs no `move-exception` and no register — it stays out of the block
    // body, matching d8. A *used* caught value becomes the handler's first body
    // instruction (allocated a register, emitted as `move-exception`); the precise-
    // interference allocator keeps it off any value live into the handler.
    //
    // ACYCLIC used-catch works (byte-identical). A LOOP used-catch is bailed: the
    // handler sits on the loop's path (handler → increment → header), and d8 also
    // shares the post-catch continuation (`s += e.hashCode()` → the catch computes
    // into the try's register and jumps back to the shared add), neither of which we
    // model — leaving it on would clobber a loop-carried value and diverge.
    // A used caught value is only problematic when the try/catch is INSIDE A LOOP — the handler's
    // continuation flows back into the guarded region (handler → … → loop header → … → try →
    // throws → handler), so the handler sits on the loop path and d8 shares the post-catch
    // continuation. That holds EXACTLY when the handler block can reach a block of its OWN try
    // region via normal successor edges (the cycle closes through the exceptional try→handler
    // edge). Handlers are laid out AFTER their try, so reaching the try forward is impossible —
    // reaching it means a back-edge, i.e. a loop. An ACYCLIC used-catch is correct even when the
    // method has an UNRELATED loop elsewhere, so check the handler's reachability, not the whole
    // method (`method_has_loop` over-bailed those). Conservative: if it reaches the try at all, bail.
    let handler_loops_back = |hb: usize| -> bool {
        let in_try = |blk: usize| {
            let pc = cfg.blocks[blk].start;
            exc_regions
                .iter()
                .any(|r| r.handler_block == hb && r.start_pc <= pc && pc < r.end_pc)
        };
        let mut seen = vec![false; cfg.blocks.len()];
        seen[hb] = true;
        let mut stack = vec![hb];
        while let Some(b) = stack.pop() {
            for &s in &cfg.blocks[b].succ {
                if in_try(s) {
                    return true;
                }
                if !seen[s] {
                    seen[s] = true;
                    stack.push(s);
                }
            }
        }
        false
    };
    let used_catch_in_loop = caught.iter().enumerate().any(|(hb, cv)| {
        cv.is_some()
            && values.iter().any(|val| {
                let mut us = operands(&val.op);
                if let SsaOp::Phi { operands, .. } = &val.op {
                    us.extend_from_slice(operands);
                }
                us.contains(&caught[hb].unwrap())
            })
            && handler_loops_back(hb)
    });
    if used_catch_in_loop {
        bail!("ssa: used catch variable in a loop (d8 shares the post-catch continuation) not yet supported");
    }
    let mut used: BTreeSet<ValId> = BTreeSet::new();
    for val in &values {
        for o in operands(&val.op) {
            used.insert(o);
        }
        if let SsaOp::Phi { operands, .. } = &val.op {
            for &o in operands {
                used.insert(o);
            }
        }
    }
    // TERMINATOR operands count as uses too — crucially a catch-all/finally handler's
    // caught exception is used ONLY by its rethrow (`Terminator::Throw`); without this it
    // wouldn't be marked used, the move-exception wouldn't emit, and the rethrow would
    // throw an undefined register (an ART VerifyError caught earlier).
    for blk in &blocks {
        for o in term_operands(&blk.term) {
            used.insert(o);
        }
    }
    for hb in 0..blocks.len() {
        if let Some(cv) = caught[hb] {
            if used.contains(&cv) {
                blocks[hb].body.insert(0, cv);
            }
        }
    }

    Ok(SsaFn { values, blocks, num_arg_registers, exc_regions, caught })
}

struct Builder<'a> {
    values: &'a mut Vec<SsaValue>,
}
impl<'a> Builder<'a> {
    fn new(&mut self, op: SsaOp, wide: bool, block: usize) -> ValId {
        let id = self.values.len() as ValId;
        self.values.push(SsaValue { id, op, wide, is_ref: false, block });
        id
    }
}

/// Pop one operand from the SSA build stack, BAILING (not panicking) on underflow — an
/// unmodeled-construct/unbalanced-stack situation should fail loudly-but-safely, never
/// crash the dexer (a panic was observed on real-world bytecode; must stay a clean bail).
macro_rules! pop_stack {
    ($s:expr) => {
        $s.pop().ok_or_else(|| anyhow::anyhow!("ssa: operand-stack underflow (unmodeled construct)"))?
    };
}

#[allow(clippy::too_many_arguments)]
fn rename(
    cf: &ClassFile,
    cfg: &Cfg,
    bc: &[u8],
    children: &[Vec<usize>],
    block_phi_slots: &[Vec<u16>],
    block_stack_phis: &[Vec<ValId>],
    exc_regions: &[ExcRegion],
    handler_phis: &[Vec<(u16, ValId)>],
    caught: &[Option<ValId>],
    exit_stacks: &mut Vec<Option<Vec<ValId>>>,
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
    // Handler-φ outputs are likewise the handler's entry version of their slot.
    for &(slot, id) in &handler_phis[blk] {
        versions.entry(slot).or_default().push(id);
        pushed.push(slot);
    }

    // The operand stack at block entry: the caught exception for a handler block,
    // the stack-merge φs for a merge block, the (already-processed, dominating)
    // single predecessor's exit stack otherwise, or empty (entry / depth-0 merge).
    let mut stack: Vec<ValId> = if let Some(cv) = caught[blk] {
        vec![cv]
    } else if !block_stack_phis[blk].is_empty() {
        block_stack_phis[blk].clone()
    } else if cfg.preds[blk].len() == 1 {
        exit_stacks[cfg.preds[blk][0]].clone().unwrap_or_default()
    } else {
        Vec::new()
    };
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
        // A throwing instruction inside a guarded region snapshots each handler-φ's
        // slot at its current version — the handler sees the state AS OF the throw,
        // not the try block's exit.
        if !exc_regions.is_empty() && is_throwing_op(op) {
            for r in exc_regions {
                if r.start_pc <= pc && pc < r.end_pc {
                    for &(slot, phi) in &handler_phis[r.handler_block] {
                        let v = cur(versions, slot)?;
                        if let SsaOp::Phi { operands, .. } = &mut b.values[phi as usize].op {
                            operands.push(v);
                        }
                    }
                }
            }
        }
        match op {
            // loads
            0x1a..=0x1d => { stack.push(cur(versions, (op - 0x1a) as u16)?); pc += 1; }
            0x1e..=0x21 => { stack.push(cur(versions, (op - 0x1e) as u16)?); pc += 1; }
            0x22..=0x25 => { stack.push(cur(versions, (op - 0x22) as u16)?); pc += 1; }
            0x26..=0x29 => { stack.push(cur(versions, (op - 0x26) as u16)?); pc += 1; }
            0x2a..=0x2d => { stack.push(cur(versions, (op - 0x2a) as u16)?); pc += 1; }
            0x15..=0x19 => { stack.push(cur(versions, bc[pc + 1] as u16)?); pc += 2; }
            // aconst_null — d8 materializes null as `const/4 v, 0` (the "zero" type
            // unifies with any reference; φ-moves use the φ's is_ref for move-object).
            0x01 => { let v = b.new(SsaOp::ConstInt(0), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            // int constants
            0x02..=0x08 => { let v = b.new(SsaOp::ConstInt(op as i32 - 0x03), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x10 => { let v = b.new(SsaOp::ConstInt(bc[pc + 1] as i8 as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 2; }
            0x11 => { let v = b.new(SsaOp::ConstInt(i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 3; }
            // long/double constants (lconst_0/1, dconst_0/1) — wide.
            0x09 | 0x0a => { let v = b.new(SsaOp::ConstLong((op - 0x09) as i64), true, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x0e => { let v = b.new(SsaOp::ConstLong(0), true, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x0f => { let v = b.new(SsaOp::ConstLong(0x3ff0_0000_0000_0000u64 as i64), true, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            // float constants (fconst_0/1/2) — narrow, the IEEE-754 bit pattern.
            0x0b => { let v = b.new(SsaOp::ConstInt(0), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x0c => { let v = b.new(SsaOp::ConstInt(0x3f80_0000u32 as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            0x0d => { let v = b.new(SsaOp::ConstInt(0x4000_0000u32 as i32), false, blk); blocks[blk].body.push(v); stack.push(v); pc += 1; }
            // ldc / ldc_w — int / float / String constant from the pool.
            0x12 | 0x13 => {
                use skotch_classfile::constant_pool::Constant;
                let idx = if op == 0x12 { bc[pc + 1] as u16 } else { u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) };
                let v = match cf.constant_pool.get(idx) {
                    Constant::Integer(c) => b.new(SsaOp::ConstInt(*c), false, blk),
                    Constant::Float(fl) => b.new(SsaOp::ConstInt(fl.to_bits() as i32), false, blk),
                    Constant::String { string_index } => {
                        let s = cf.constant_pool.utf8(*string_index)?.to_string();
                        b.new(SsaOp::ConstString { value: s, jvm_pc: pc as u32 }, false, blk)
                    }
                    Constant::Class { .. } => {
                        let desc = crate::bootstrap::class_ref_desc(cf, idx)?;
                        b.new(SsaOp::ConstClass { type_desc: desc, jvm_pc: pc as u32 }, false, blk)
                    }
                    _ => bail!("ssa: unsupported ldc constant (methodhandle/methodtype)"),
                };
                blocks[blk].body.push(v);
                stack.push(v);
                pc += if op == 0x12 { 2 } else { 3 };
            }
            // ldc2_w — long / double constant from the pool (wide).
            0x14 => {
                use skotch_classfile::constant_pool::Constant;
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let v = match cf.constant_pool.get(idx) {
                    Constant::Long(c) => b.new(SsaOp::ConstLong(*c), true, blk),
                    Constant::Double(d) => b.new(SsaOp::ConstLong(d.to_bits() as i64), true, blk),
                    _ => bail!("ssa: bad ldc2 constant"),
                };
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 3;
            }
            // stores: rename the slot to the popped value (no instruction)
            0x36..=0x3a => { let v = pop_stack!(stack); versions.entry(bc[pc + 1] as u16).or_default().push(v); pushed.push(bc[pc + 1] as u16); pc += 2; }
            0x3b..=0x3e => { let v = pop_stack!(stack); let s = (op - 0x3b) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x3f..=0x42 => { let v = pop_stack!(stack); let s = (op - 0x3f) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x43..=0x46 => { let v = pop_stack!(stack); let s = (op - 0x43) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x47..=0x4a => { let v = pop_stack!(stack); let s = (op - 0x47) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            0x4b..=0x4e => { let v = pop_stack!(stack); let s = (op - 0x4b) as u16; versions.entry(s).or_default().push(v); pushed.push(s); pc += 1; }
            // iinc slot, const → slot = slot + const
            0x84 => {
                let slot = bc[pc + 1] as u16;
                let c = bc[pc + 2] as i8 as i32;
                let cst = b.new(SsaOp::ConstInt(c), false, blk);
                blocks[blk].body.push(cst);
                let old = cur(versions, slot)?;
                let sum = b.new(SsaOp::Binop { jvm_op: 0x60, a: old, b: cst, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(sum);
                versions.entry(slot).or_default().push(sum);
                pushed.push(slot);
                pc += 3;
            }
            // arithmetic/bitwise/shift binops (int/long/float/double). Integer/long
            // div/rem are throwing — `jvm_pc` lets the emitter record a position.
            0x60..=0x73 | 0x78..=0x83 => {
                let rb = pop_stack!(stack);
                let ra = pop_stack!(stack);
                let v = b.new(SsaOp::Binop { jvm_op: op, a: ra, b: rb, jvm_pc: pc as u32 }, binop_result_wide(op), blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // unary negation (ineg/lneg/fneg/dneg) — result width = operand width.
            0x74..=0x77 => {
                let a = pop_stack!(stack);
                let wide = op == 0x75 || op == 0x77;
                let v = b.new(SsaOp::Unop { jvm_op: op, a }, wide, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // numeric conversions (i2l..i2s). Result width per the target type.
            0x85..=0x93 => {
                let a = pop_stack!(stack);
                let v = b.new(SsaOp::Unop { jvm_op: op, a }, conv_result_wide(op), blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // comparisons (produce a narrow result, used by a following branch)
            0x94..=0x98 => {
                let rb = pop_stack!(stack);
                let ra = pop_stack!(stack);
                let v = b.new(SsaOp::Cmp { jvm_op: op, a: ra, b: rb }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // conditional branches (int if<cond>z, if_icmp<cond>, and ref
            // ifnull/ifnonnull — the latter compare an object to null = if-eqz/if-nez)
            0x99..=0xa6 | 0xc6 | 0xc7 => {
                let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                let two = (0x9f..=0xa6).contains(&op);
                let operands = if two {
                    let r = pop_stack!(stack);
                    let l = pop_stack!(stack);
                    vec![l, r]
                } else {
                    vec![pop_stack!(stack)]
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
            // tableswitch / lookupswitch — key on top of stack; lowered to an if-eq chain.
            0xaa | 0xab => {
                let value = pop_stack!(stack);
                let (default_pc, raw_cases, end) = crate::bootstrap::parse_switch(bc, pc);
                let blk_at = |p: usize| cfg.blocks.iter().position(|x| x.start == p).unwrap();
                let default = blk_at(default_pc);
                let cases = raw_cases.iter().map(|&(k, t)| (k, blk_at(t))).collect();
                term = Some(Terminator::Switch { value, default, cases });
                pc = end;
            }
            0xb1 => { term = Some(Terminator::Return { value: None, op: 0x0e }); pc += 1; }
            // ireturn/lreturn/freturn/dreturn/areturn → return / return-wide / return-object
            0xac | 0xad | 0xae | 0xaf | 0xb0 => {
                let rop = match op { 0xad | 0xaf => 0x10, 0xb0 => 0x11, _ => 0x0f };
                term = Some(Terminator::Return { value: Some(pop_stack!(stack)), op: rop });
                pc += 1;
            }
            // athrow → `throw v` — ends the block; the handler-φ snapshot at line ~1083
            // already captured locals (athrow is a throwing op), so no successor wiring.
            0xbf => { term = Some(Terminator::Throw { value: pop_stack!(stack), jvm_pc: pc as u32 }); pc += 1; }
            // method calls: invokevirtual/special/static/interface
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let (class, name, desc) = cf.constant_pool.member_ref(idx)?;
                let (mparams, ret) = crate::bootstrap::parse_descriptor(&desc)?;
                let is_static = op == 0xb8;
                let argc = mparams.len() + if is_static { 0 } else { 1 };
                let mut args: Vec<ValId> = Vec::with_capacity(argc);
                for _ in 0..argc {
                    args.push(pop_stack!(stack));
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
            // invokedynamic — ONLY string concatenation (StringConcatFactory
            // makeConcat/makeConcatWithConstants) is supported, DESUGARED here to a
            // `new StringBuilder; append…; toString()` chain of ordinary SsaOps (so emit/
            // regalloc need no changes). Any other indy (lambda metafactory, …) bails.
            0xba => {
                use skotch_classfile::constant_pool::Constant;
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let (bsm_idx, nt_idx) = match cf.constant_pool.get(idx) {
                    Constant::InvokeDynamic { bootstrap_method_attr_index, name_and_type_index } => {
                        (*bootstrap_method_attr_index, *name_and_type_index)
                    }
                    _ => bail!("ssa: indy constant {idx} is not InvokeDynamic"),
                };
                let (name, desc) = cf.constant_pool.name_and_type(nt_idx)?;
                let (name, desc) = (name.to_string(), desc.to_string());
                // LAMBDA metafactory: desugar to a synthetic class. Non-capturing → load its
                // singleton INSTANCE; capturing → new-instance + invoke-direct with the captured
                // values popped off the stack. Returns None for other bootstraps (e.g. concat).
                match crate::lambda::try_lambda_metafactory(cf, idx)? {
                    Some(crate::lambda::LambdaSite::Singleton(field)) => {
                        let v = b.new(SsaOp::GetStatic { dex_op: 0x62, field, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(v);
                        stack.push(v);
                        pc += 5;
                        continue;
                    }
                    Some(crate::lambda::LambdaSite::Capturing { class, ctor, captures }) => {
                        let mut cap_args: Vec<ValId> = Vec::with_capacity(captures.len());
                        for _ in 0..captures.len() {
                            cap_args.push(pop_stack!(stack));
                        }
                        cap_args.reverse();
                        let obj = b.new(SsaOp::NewInstance { type_desc: class, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(obj);
                        let mut init_args = vec![obj];
                        init_args.extend(cap_args);
                        let init = b.new(SsaOp::Invoke { dex_op: 0x70, method: ctor, args: init_args, ret: None, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(init);
                        stack.push(obj);
                        pc += 5;
                        continue;
                    }
                    None => {}
                }
                if name != "makeConcatWithConstants" && name != "makeConcat" {
                    bail!("ssa: unsupported invokedynamic '{name}' (only string-concat)");
                }
                let (param_tys, _ret) = crate::bootstrap::parse_descriptor(&desc)?;
                let mut args: Vec<ValId> = Vec::with_capacity(param_tys.len());
                for _ in 0..param_tys.len() {
                    args.push(pop_stack!(stack));
                }
                args.reverse();
                let bsm = &cf.bootstrap_methods[bsm_idx as usize];
                // makeConcatWithConstants: arg[0] is the recipe String (=arg,
                // =constant from arg[1..], else literal). makeConcat: implicit *n.
                let recipe: String = if name == "makeConcatWithConstants" {
                    match cf.constant_pool.get(bsm.arguments[0]) {
                        Constant::String { string_index } => cf.constant_pool.utf8(*string_index)?.to_string(),
                        _ => bail!("ssa: string-concat recipe is not a String constant"),
                    }
                } else {
                    "\u{1}".repeat(param_tys.len())
                };
                // Parse the recipe into append pieces.
                enum Piece { Lit(String), Arg, Const(u16) }
                let mut pieces: Vec<Piece> = Vec::new();
                let mut lit = String::new();
                let mut const_i = 1usize;
                for ch in recipe.chars() {
                    match ch {
                        '\u{1}' => {
                            if !lit.is_empty() { pieces.push(Piece::Lit(std::mem::take(&mut lit))); }
                            pieces.push(Piece::Arg);
                        }
                        '\u{2}' => {
                            if !lit.is_empty() { pieces.push(Piece::Lit(std::mem::take(&mut lit))); }
                            let c = bsm.arguments[const_i];
                            const_i += 1;
                            pieces.push(Piece::Const(c));
                        }
                        c => lit.push(c),
                    }
                }
                if !lit.is_empty() { pieces.push(Piece::Lit(lit)); }
                // `new StringBuilder` + `<init>()`.
                let sb_desc = "Ljava/lang/StringBuilder;".to_string();
                let sb = b.new(SsaOp::NewInstance { type_desc: sb_desc.clone(), jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(sb);
                let init = b.new(
                    SsaOp::Invoke {
                        dex_op: 0x70,
                        method: MethodRef { class: sb_desc.clone(), proto: ProtoRef { return_type: "V".into(), params: vec![] }, name: "<init>".into() },
                        args: vec![sb],
                        ret: None,
                        jvm_pc: pc as u32,
                    },
                    false,
                    blk,
                );
                blocks[blk].body.push(init);
                // `StringBuilder.append(<piece>)` for each piece. append returns `this`
                // (the builder is mutated in place), so we DISCARD the result (ret: None)
                // and keep using `sb`.
                let append_param = |jvm: &str| -> &'static str {
                    match jvm {
                        "I" | "B" | "S" => "I",
                        "J" => "J",
                        "F" => "F",
                        "D" => "D",
                        "Z" => "Z",
                        "C" => "C",
                        "Ljava/lang/String;" => "Ljava/lang/String;",
                        _ => "Ljava/lang/Object;",
                    }
                };
                let mut arg_i = 0usize;
                for piece in pieces {
                    let (val, ptype): (ValId, String) = match piece {
                        Piece::Lit(s) => {
                            let cv = b.new(SsaOp::ConstString { value: s, jvm_pc: pc as u32 }, false, blk);
                            blocks[blk].body.push(cv);
                            (cv, "Ljava/lang/String;".to_string())
                        }
                        Piece::Arg => {
                            let v = args[arg_i];
                            let pt = append_param(&param_tys[arg_i]).to_string();
                            arg_i += 1;
                            (v, pt)
                        }
                        Piece::Const(c) => match cf.constant_pool.get(c) {
                            Constant::String { string_index } => {
                                let s = cf.constant_pool.utf8(*string_index)?.to_string();
                                let cv = b.new(SsaOp::ConstString { value: s, jvm_pc: pc as u32 }, false, blk);
                                blocks[blk].body.push(cv);
                                (cv, "Ljava/lang/String;".to_string())
                            }
                            Constant::Integer(n) => {
                                let cv = b.new(SsaOp::ConstInt(*n), false, blk);
                                blocks[blk].body.push(cv);
                                (cv, "I".to_string())
                            }
                            _ => bail!("ssa: string-concat \\u0002 constant kind unsupported"),
                        },
                    };
                    let ap = b.new(
                        SsaOp::Invoke {
                            dex_op: 0x6e,
                            method: MethodRef { class: sb_desc.clone(), proto: ProtoRef { return_type: sb_desc.clone(), params: vec![ptype] }, name: "append".into() },
                            args: vec![sb, val],
                            ret: None,
                            jvm_pc: pc as u32,
                        },
                        false,
                        blk,
                    );
                    blocks[blk].body.push(ap);
                }
                // `toString()` → the concatenated String result.
                let result = b.new(
                    SsaOp::Invoke {
                        dex_op: 0x6e,
                        method: MethodRef { class: sb_desc, proto: ProtoRef { return_type: "Ljava/lang/String;".into(), params: vec![] }, name: "toString".into() },
                        args: vec![sb],
                        ret: Some(RetKind { wide: false, is_ref: true }),
                        jvm_pc: pc as u32,
                    },
                    false,
                    blk,
                );
                blocks[blk].body.push(result);
                stack.push(result);
                pc += 5;
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
                        let obj = pop_stack!(stack);
                        let v = b.new(SsaOp::GetField { dex_op: crate::bootstrap::iget_op(&desc), field, obj, jvm_pc: pc as u32 }, wide, blk);
                        blocks[blk].body.push(v);
                        stack.push(v);
                    }
                    0xb3 => {
                        let value = pop_stack!(stack);
                        let v = b.new(SsaOp::PutStatic { dex_op: crate::bootstrap::sput_op(&desc), field, value, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(v);
                    }
                    0xb5 => {
                        let value = pop_stack!(stack);
                        let obj = pop_stack!(stack);
                        let v = b.new(SsaOp::PutField { dex_op: crate::bootstrap::iput_op(&desc), field, obj, value, jvm_pc: pc as u32 }, false, blk);
                        blocks[blk].body.push(v);
                    }
                    _ => unreachable!(),
                }
                pc += 3;
            }
            // monitorenter (0xc2) / monitorexit (0xc3) — `synchronized` lock acquire/release.
            // A statement (pops the monitor objectref, defines no value); throwing.
            0xc2 | 0xc3 => {
                let obj = pop_stack!(stack);
                let v = b.new(SsaOp::Monitor { enter: op == 0xc2, obj, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                pc += 1;
            }
            // array element load: iaload/laload/faload/daload/aaload/baload/caload/saload
            0x2e..=0x35 => {
                let (dex_op, wide) = crate::bootstrap::aget_op(op);
                let index = pop_stack!(stack);
                let array = pop_stack!(stack);
                let v = b.new(SsaOp::ArrayGet { dex_op, array, index, jvm_pc: pc as u32 }, wide, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // array element store: i/l/f/d/a/b/c/sastore
            0x4f..=0x56 => {
                let dex_op = crate::bootstrap::aput_op(op);
                let value = pop_stack!(stack);
                let index = pop_stack!(stack);
                let array = pop_stack!(stack);
                let v = b.new(SsaOp::ArrayPut { dex_op, array, index, value, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                pc += 1;
            }
            // arraylength
            0xbe => {
                let array = pop_stack!(stack);
                let v = b.new(SsaOp::ArrayLength { array, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 1;
            }
            // new-instance: `new X` pushes an uninitialized ref (a later <init> call
            // initializes it in place).
            0xbb => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let internal = cf.constant_pool.class_name(idx)?.to_string();
                let desc = skotch_classfile::constant_pool::internal_to_descriptor(&internal);
                let v = b.new(SsaOp::NewInstance { type_desc: desc, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 3;
            }
            // newarray (primitive element) / anewarray (reference element)
            0xbc | 0xbd => {
                let desc = if op == 0xbc {
                    crate::bootstrap::newarray_desc(bc[pc + 1]).to_string()
                } else {
                    let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                    format!("[{}", crate::bootstrap::class_ref_desc(cf, idx)?)
                };
                let size = pop_stack!(stack);
                let v = b.new(SsaOp::NewArray { type_desc: desc, size, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += if op == 0xbc { 2 } else { 3 };
            }
            // dup: duplicate the top stack value (the `new X; dup; <init>` idiom).
            0x59 => {
                let top = *stack.last().ok_or_else(|| anyhow::anyhow!("ssa: dup on empty operand stack"))?;
                stack.push(top);
                pc += 1;
            }
            // swap: exchange the top two category-1 values — pure value-stack reorder.
            0x5f => {
                let n = stack.len();
                if n < 2 {
                    bail!("ssa: swap with fewer than 2 operands");
                }
                stack.swap(n - 2, n - 1);
                pc += 1;
            }
            // dup_x1: `..., v2, v1 → ..., v1, v2, v1` — duplicate top below the second.
            0x5a => {
                let v1 = pop_stack!(stack);
                let v2 = pop_stack!(stack);
                stack.push(v1);
                stack.push(v2);
                stack.push(v1);
                pc += 1;
            }
            // dup_x2: insert a copy of the cat-1 top below the next one OR two values.
            // Form 2 (value below is category-2/wide): `w, v1 → v1, w, v1`.
            // Form 1 (two category-1 below): `v3, v2, v1 → v1, v3, v2, v1`.
            // Pure value-stack reorder (no instruction) — the duplicated value-id is reused.
            0x5b => {
                let v1 = pop_stack!(stack);
                let below = *stack.last().ok_or_else(|| anyhow::anyhow!("ssa: dup_x2 underflow"))?;
                if b.values[below as usize].wide {
                    let w = pop_stack!(stack);
                    stack.push(v1);
                    stack.push(w);
                    stack.push(v1);
                } else {
                    let v2 = pop_stack!(stack);
                    let v3 = pop_stack!(stack);
                    stack.push(v1);
                    stack.push(v3);
                    stack.push(v2);
                    stack.push(v1);
                }
                pc += 1;
            }
            // dup2_x1: Form 2 (category-2/wide top): `v2, w → w, v2, w`.
            // Form 1 (two category-1 on top): `v3, v2, v1 → v2, v1, v3, v2, v1`.
            0x5d => {
                let top = *stack.last().ok_or_else(|| anyhow::anyhow!("ssa: dup2_x1 underflow"))?;
                if b.values[top as usize].wide {
                    let w = pop_stack!(stack);
                    let v2 = pop_stack!(stack);
                    stack.push(w);
                    stack.push(v2);
                    stack.push(w);
                } else {
                    let v1 = pop_stack!(stack);
                    let v2 = pop_stack!(stack);
                    let v3 = pop_stack!(stack);
                    stack.push(v2);
                    stack.push(v1);
                    stack.push(v3);
                    stack.push(v2);
                    stack.push(v1);
                }
                pc += 1;
            }
            // dup2: a wide (category-2) top duplicates that one value; else duplicates the
            // top TWO category-1 values — `a[i]++`/`a[i]+=x` dup the array+index so one
            // aget + one aput share them (the SSA values are reused, no new instruction).
            0x5c => {
                let top = *stack.last().ok_or_else(|| anyhow::anyhow!("ssa: dup2 on empty operand stack"))?;
                if b.values[top as usize].wide {
                    stack.push(top);
                } else {
                    let n = stack.len();
                    if n < 2 {
                        bail!("ssa: dup2 with fewer than 2 category-1 operands");
                    }
                    let (v2, v1) = (stack[n - 2], stack[n - 1]);
                    stack.push(v2);
                    stack.push(v1);
                }
                pc += 1;
            }
            // pop / pop2: discard. A discarded call result drops its move-result, so
            // mark such an Invoke as void (d8 emits the call alone).
            0x57 => {
                let v = pop_stack!(stack);
                if let SsaOp::Invoke { ret, .. } = &mut b.values[v as usize].op {
                    *ret = None;
                }
                pc += 1;
            }
            0x58 => {
                let v = pop_stack!(stack);
                if !b.values[v as usize].wide {
                    stack.pop();
                }
                if let SsaOp::Invoke { ret, .. } = &mut b.values[v as usize].op {
                    *ret = None;
                }
                pc += 1;
            }
            // checkcast — `check-cast vAA, type@`. Throwing (ClassCastException). The
            // result aliases the object (in-place in DEX); emission inserts a move if
            // the result register didn't coalesce with the object's.
            0xc0 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let desc = crate::bootstrap::class_ref_desc(cf, idx)?;
                let obj = pop_stack!(stack);
                let v = b.new(SsaOp::CheckCast { obj, type_desc: desc, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 3;
            }
            // instanceof — `instance-of vA, vB, type@`. Non-throwing; int (boolean) result.
            0xc1 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let desc = crate::bootstrap::class_ref_desc(cf, idx)?;
                let obj = pop_stack!(stack);
                let v = b.new(SsaOp::InstanceOf { obj, type_desc: desc, jvm_pc: pc as u32 }, false, blk);
                blocks[blk].body.push(v);
                stack.push(v);
                pc += 3;
            }
            0x00 => { pc += 1; } // nop — d8 drops it
            other => bail!("ssa: unsupported opcode {other:#04x} (loop subset only)"),
        }
    }
    // The remaining stack values flow to successors (e.g. a ternary's result on a
    // branch arm); a successor merge resolves them via its stack-merge φs.
    exit_stacks[blk] = Some(stack);
    // A block with no explicit terminator falls through to its single successor.
    blocks[blk].term = term.unwrap_or_else(|| {
        Terminator::Fall { target: cfg.blocks[blk].succ.first().copied().unwrap_or(blk) }
    });

    // Fill φ operands in successors for this predecessor edge — local φs from the
    // current versions, stack-merge φs from this block's exit stack (by position).
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
        let sphis = block_stack_phis[s].clone();
        for (p, &phi_id) in sphis.iter().enumerate() {
            let operand = exit_stacks[blk].as_ref().unwrap()[p];
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
        rename(cf, cfg, bc, children, block_phi_slots, block_stack_phis, exc_regions, handler_phis, caught, exit_stacks, blocks, b, versions, c)?;
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
    // Layout = blocks reachable from the entry (via normal successor edges) PLUS each live
    // try region's handler (reached via exception, not a succ edge), in block-index order. This
    // EXCLUDES a dead handler block whose region was dropped (a try/finally over a non-throwing
    // body) — emitting it would place a `move-exception` outside any catch (an ART VerifyError).
    // For methods with no unreachable blocks the filter keeps all blocks → byte-identical.
    let n = f.blocks.len();
    let mut reachable = vec![false; n];
    let mut stack: Vec<usize> = vec![0];
    for r in &f.exc_regions {
        stack.push(r.handler_block);
    }
    while let Some(b) = stack.pop() {
        if std::mem::replace(&mut reachable[b], true) {
            continue;
        }
        for &s in &f.blocks[b].succ {
            if !reachable[s] {
                stack.push(s);
            }
        }
    }
    let layout: Vec<usize> = (0..n).filter(|&b| reachable[b]).collect();
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

/// Per-block live-in / live-out value sets (backward dataflow). A φ operand is
/// live-out of exactly the predecessor edge it comes from (NOT a general live-in
/// of the φ's block) — so a value that is ONLY a φ operand is not live-in of the
/// φ's block, which the coalescer relies on.
pub(crate) fn block_liveness(f: &SsaFn) -> (Vec<BTreeSet<ValId>>, Vec<BTreeSet<ValId>>) {
    let n = f.blocks.len();
    let mut use_: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    let mut def_: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    for b in 0..n {
        let blk = &f.blocks[b];
        let mut defined: BTreeSet<ValId> = BTreeSet::new();
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
    let mut live_in: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    let mut live_out: Vec<BTreeSet<ValId>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for b in (0..n).rev() {
            let mut lo: BTreeSet<ValId> = BTreeSet::new();
            for &s in &f.blocks[b].succ {
                for &v in &live_in[s] {
                    lo.insert(v);
                }
                let pred_idx = f.blocks[s].preds.iter().position(|&p| p == b).unwrap();
                for &phi in &f.blocks[s].phis {
                    if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                        if let Some(&opnd) = operands.get(pred_idx) {
                            lo.insert(opnd);
                        }
                    }
                }
            }
            // Exceptional edges: a throw in `b` reaches its handler. Everything live
            // at the handler's entry, plus every handler-φ operand (the throw-point
            // snapshots `b` contributes), is live-out of `b`.
            for &h in &f.blocks[b].exc_succ {
                for &v in &live_in[h] {
                    lo.insert(v);
                }
                for &phi in &f.blocks[h].phis {
                    if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                        for &opnd in operands {
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
    (live_in, live_out)
}

/// Computes per-value live intervals. live-in/out are found by backward dataflow
/// over the CFG (including back-edges, so loop-carried values stay live across
/// the whole loop); intervals then span each value's def to its last live point.
pub(crate) fn live_intervals(f: &SsaFn, num: &Numbering) -> Vec<Interval> {
    let n = f.blocks.len();
    let (live_in, live_out) = block_liveness(f);
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

/// Per-value PRECISE live ranges: one (start,end) segment per block the value is
/// live in, over the global numbering. Unlike `live_intervals`' single coarse
/// [min,max] span, these carry the holes needed for an exact interference test —
/// two values interfere iff a segment of one overlaps a segment of the other. The
/// distinction matters for φ-coalescing: an acyclic merge-φ's operands (e.g.
/// clamp's `x` and `lo`, both live at the `if`) interfere and must NOT share a
/// register, whereas a loop-φ's operands (init in the preheader vs the back-edge
/// value in the latch) live in DIFFERENT blocks and don't interfere — a single
/// [min,max] span (extended over the loop) can't tell them apart.
fn live_ranges(f: &SsaFn, num: &Numbering) -> Vec<Vec<(u32, u32)>> {
    let n = f.blocks.len();
    let (live_in, live_out) = block_liveness(f);
    let nv = f.values.len();
    let mut ranges: Vec<Vec<(u32, u32)>> = vec![Vec::new(); nv];
    for b in 0..n {
        let (bstart, bend) = num.block_span[b];
        // Last-use position of each value within this block, including φ-operand uses
        // contributed across this block's outgoing edges (modeled at the block end).
        let mut last_use: BTreeMap<ValId, u32> = BTreeMap::new();
        let mut pos = bstart + NUMBER_DELTA;
        for &v in &f.blocks[b].body {
            for u in operands(&f.values[v as usize].op) {
                last_use.insert(u, pos);
            }
            pos += NUMBER_DELTA;
        }
        for u in term_operands(&f.blocks[b].term) {
            last_use.insert(u, bend);
        }
        for &s in &f.blocks[b].succ {
            if let Some(pi) = f.blocks[s].preds.iter().position(|&p| p == b) {
                for &phi in &f.blocks[s].phis {
                    if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                        if let Some(&o) = operands.get(pi) {
                            last_use.insert(o, bend);
                        }
                    }
                }
            }
        }
        // Values live somewhere in this block: live-in, defined here, or used here.
        let mut cands: BTreeSet<ValId> = live_in[b].clone();
        cands.extend(f.blocks[b].phis.iter().copied());
        for &v in &f.blocks[b].body {
            if produces_value(&f.values[v as usize].op) {
                cands.insert(v);
            }
        }
        cands.extend(last_use.keys().copied());
        for v in cands {
            let lo = if live_in[b].contains(&v) { bstart } else { num.def[v as usize] };
            let hi = if live_out[b].contains(&v) {
                bend
            } else {
                last_use.get(&v).copied().unwrap_or(lo)
            };
            if hi >= lo {
                ranges[v as usize].push((lo, hi));
            }
        }
    }
    ranges
}

/// Whether two sets of live-range segments overlap. Half-open at the shared
/// endpoint: a value's last use and another's def at the SAME position (the result
/// of `r = op a, b` reusing a dying operand's register — d8's 2addr) do NOT
/// interfere; genuine simultaneous liveness (both live strictly across a point) does.
fn segs_interfere(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    for &(alo, ahi) in a {
        for &(blo, bhi) in b {
            if alo < bhi && blo < ahi {
                return true;
            }
        }
    }
    false
}

/// Whether two values' precise live ranges overlap (they cannot share a register).
fn ranges_interfere(ranges: &[Vec<(u32, u32)>], a: ValId, b: ValId) -> bool {
    a != b && segs_interfere(&ranges[a as usize], &ranges[b as usize])
}

/// The value operands an op reads.
fn operands(op: &SsaOp) -> Vec<ValId> {
    match op {
        SsaOp::Phi { .. } | SsaOp::Argument { .. } | SsaOp::ConstInt(_) | SsaOp::ConstLong(_) | SsaOp::ConstString { .. } | SsaOp::ConstClass { .. } => Vec::new(),
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
        SsaOp::NewInstance { .. } => Vec::new(),
        SsaOp::NewArray { size, .. } => vec![*size],
        SsaOp::CheckCast { obj, .. } | SsaOp::InstanceOf { obj, .. } => vec![*obj],
        SsaOp::Monitor { obj, .. } => vec![*obj],
        SsaOp::CaughtException => Vec::new(),
    }
}

/// The JVM pc of an op that carries one (for mapping throwing instructions to DEX
/// addresses when narrowing try_item ranges).
fn op_jvm_pc(op: &SsaOp) -> Option<u32> {
    match op {
        SsaOp::Invoke { jvm_pc, .. }
        | SsaOp::GetField { jvm_pc, .. }
        | SsaOp::GetStatic { jvm_pc, .. }
        | SsaOp::PutField { jvm_pc, .. }
        | SsaOp::PutStatic { jvm_pc, .. }
        | SsaOp::ArrayGet { jvm_pc, .. }
        | SsaOp::ArrayPut { jvm_pc, .. }
        | SsaOp::ArrayLength { jvm_pc, .. }
        | SsaOp::NewInstance { jvm_pc, .. }
        | SsaOp::NewArray { jvm_pc, .. }
        | SsaOp::ConstString { jvm_pc, .. }
        | SsaOp::ConstClass { jvm_pc, .. }
        | SsaOp::CheckCast { jvm_pc, .. }
        | SsaOp::InstanceOf { jvm_pc, .. }
        | SsaOp::Monitor { jvm_pc, .. }
        | SsaOp::Binop { jvm_pc, .. } => Some(*jvm_pc),
        _ => None,
    }
}

/// Whether an emitted SSA op can throw (mirrors `is_throwing_op` over the ops the
/// SSA path produces) — used to narrow a try_item to its guarded instructions.
fn ssa_op_can_throw(op: &SsaOp) -> bool {
    match op {
        SsaOp::Invoke { .. }
        | SsaOp::GetField { .. }
        | SsaOp::GetStatic { .. }
        | SsaOp::PutField { .. }
        | SsaOp::PutStatic { .. }
        | SsaOp::ArrayGet { .. }
        | SsaOp::ArrayPut { .. }
        | SsaOp::ArrayLength { .. }
        | SsaOp::NewInstance { .. }
        | SsaOp::NewArray { .. }
        | SsaOp::CheckCast { .. }
        | SsaOp::ConstString { .. }
        | SsaOp::ConstClass { .. }
        | SsaOp::Monitor { .. } => true,
        SsaOp::Binop { jvm_op, .. } => matches!(jvm_op, 0x6c | 0x6d | 0x70 | 0x71),
        _ => false,
    }
}

/// Whether an arithmetic/bitwise/shift binop produces a wide (long/double) result.
fn binop_result_wide(jvm_op: u8) -> bool {
    matches!(
        jvm_op,
        // long: ladd/lsub/lmul/ldiv/lrem, land/lor/lxor, lshl/lshr/lushr
        0x61 | 0x65 | 0x69 | 0x6d | 0x71 | 0x7f | 0x81 | 0x83 | 0x79 | 0x7b | 0x7d
        // double: dadd/dsub/dmul/ddiv/drem
        | 0x63 | 0x67 | 0x6b | 0x6f | 0x73
    )
}

/// Whether a numeric conversion (i2l..i2s) produces a wide (long/double) result.
fn conv_result_wide(jvm_op: u8) -> bool {
    matches!(
        jvm_op,
        0x85 /*i2l*/ | 0x87 /*i2d*/ | 0x8a /*l2d*/ | 0x8c /*f2l*/ | 0x8d /*f2d*/ | 0x8f /*d2l*/
    )
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
            | SsaOp::Monitor { .. }
    )
}

fn term_operands(t: &Terminator) -> Vec<ValId> {
    match t {
        Terminator::If { operands, .. } => operands.clone(),
        Terminator::Return { value: Some(v), .. } => vec![*v],
        Terminator::Throw { value, .. } => vec![*value],
        Terminator::Switch { value, .. } => vec![*value],
        _ => Vec::new(),
    }
}

// ──────────────────── linear-scan register allocation ────────────────────

/// Per-value register assignment in d8's "allocated space" (args at
/// `[0, num_arg_registers)`; the allocated→real args-high remap is applied later
/// by `crate::regalloc`). Coalesced values (a φ and its operands; an in-place
/// update and its source) share a register so loop-carried values need no moves.
#[derive(Clone)]
pub(crate) struct Allocation {
    /// allocated register per value id (NO_REG for rematerialized constants).
    pub(crate) reg: Vec<u16>,
    pub(crate) registers_used: u16,
}

pub(crate) const NO_REG: u16 = u16::MAX;

/// Reserve `k` low scratch registers for the >16-register spill pass. The args-high remap
/// (`regalloc::remap_register`) maps a LOCAL allocated register `r` (`r >= num_arg`) to the FINAL
/// register `r - num_arg` and an ARGUMENT (`r < num_arg`) to a high one; so shifting every local's
/// allocated register up by `k` leaves the allocated slots `[num_arg, num_arg + k)` unused — and
/// those map to the LOWEST final registers `0..k`, the ≤15 scratch a spill needs to move an
/// unwidenable nibble operand (iget/iput/…) whose own final register is ≥16. Arguments keep their
/// allocated slots (and stay high after remap); `registers_used` grows by `k` so the frame covers
/// the shift. This is a pure renumbering — it preserves the allocation's correctness (no two live
/// values collide, since every local moved by the same `k`) and is the identity when `k == 0`.
/// (Used by the >16-register iget/iput spill in `dex_method_ssa`.)
pub(crate) fn reserve_scratch(alloc: &mut Allocation, num_arg: u16, k: u16) {
    if k == 0 {
        return;
    }
    for r in alloc.reg.iter_mut() {
        if *r != NO_REG && *r >= num_arg {
            *r += k;
        }
    }
    alloc.registers_used += k;
}

/// Whether a constant value is *rematerialized* (folded as a literal) rather
/// than allocated: a small int constant used only by lit-foldable ops, mirroring
/// d8's const handling (`iinc`'s constant becomes `add-int/lit8`, no register).
fn is_rematerialized(f: &SsaFn, v: ValId) -> bool {
    let val = &f.values[v as usize];
    let c = match val.op {
        SsaOp::ConstInt(c) => c,
        _ => return false,
    };
    // Foldable into a lit8 (`x op #-128..127`) or lit16 (`x op #-32768..32767`) form;
    // all `lit_ops` have both, and emit_binop picks the narrower that fits. A const
    // outside lit16 range can't fold, so it keeps its own register.
    if !(-32768..=32767).contains(&c) {
        return false;
    }
    // EVERY use must be a lit-foldable binop operand: as the RIGHT operand, or the LEFT
    // operand of a COMMUTATIVE op (d8 folds `3*n` as `mul-int/lit8 n,#3`). ANY other use
    // (array index/element, field obj, call arg, cmp/unop operand, φ operand, new-array
    // size, a branch/return value) needs the const in a register — rematerializing it
    // would leave that operand with NO_REG (a miscompile: e.g. `aget v2,v3,vNO_REG`).
    let mut any_use = false;
    for u in &f.values {
        match &u.op {
            SsaOp::Binop { jvm_op, a, b, .. } => {
                let (jop, a, b) = (*jvm_op, *a, *b);
                let on_left = a == v;
                let on_right = b == v;
                if !on_left && !on_right {
                    continue;
                }
                // `c op c` needs the const in a register (the lit-fold's source) AND as
                // the literal. A const LEFT of a NON-commutative op can't fold — EXCEPT
                // isub (`c - x`), which DEX folds via rsub-int (reverse subtract). Other
                // non-commutative left (shift/div/rem) keep the const's register.
                let isub_left = jop == 0x64 && on_left;
                if (on_left && on_right)
                    || (on_left && !crate::bootstrap::is_commutative(jop) && !isub_left)
                {
                    return false;
                }
                any_use = true;
                // isub: `x - c` folds as `x + (-c)` (NEGATED const must fit); `c - x` folds
                // as `rsub-int x, #c` (the const itself, fits the outer lit16 range). Shifts
                // are lit8-only; others use lit_ops (commutative left-const folds via swap).
                let foldable = if jop == 0x64 {
                    if on_left {
                        (-32768..=32767).contains(&(c as i64))
                    } else {
                        (-32768..=32767).contains(&-(c as i64))
                    }
                } else if crate::bootstrap::shift_lit8_op(jop).is_some() {
                    (-128..=127).contains(&c)
                } else {
                    crate::bootstrap::lit_ops(jop).is_some()
                };
                if !foldable {
                    return false;
                }
            }
            SsaOp::Phi { operands, .. } => {
                if operands.contains(&v) {
                    return false;
                }
            }
            other => {
                if operands(other).contains(&v) {
                    return false;
                }
            }
        }
    }
    // A const read by an if-test or returned directly also needs a register.
    for blk in &f.blocks {
        if term_operands(&blk.term).contains(&v) {
            return false;
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
    let (live_in, live_out) = block_liveness(f);
    let ranges = live_ranges(f, num);
    // 1. Coalesce φ-nodes with their operands, subject to two interference checks:
    //  (a) φ vs operand: skip an operand that is also live-in of the φ's block (it's
    //      independently live past the merge — e.g. a loop counter a conditional
    //      reassignment reads but that lives on to `i++`; coalescing would clobber
    //      it, so leave it for a φ-move / bail rather than miscompile).
    //  (b) φ vs operand, live-OUT: skip an operand that is live-OUT of the φ's block
    //      — it stays live past the φ's definition, so it coexists with (interferes
    //      with) the φ. This catches a φ whose operand is ANOTHER φ in the same block
    //      that is itself live (e.g. Fibonacci `a=b; b=t`: the a-φ's back-edge operand
    //      is the b-φ, which is live across the loop body — coalescing a and b into one
    //      register is a miscompile, `add v0,v0`). The live-IN check misses it because
    //      the b-φ is DEFINED in the header (not live-in there).
    //  (c) operand vs operand: skip an operand that interferes (precise live-range
    //      overlap) with one already coalesced into THIS φ's group — e.g. clamp's
    //      `if(r<lo)r=lo` merge-φ has operands `x` and `lo` that are both live at the
    //      `if(x<lo)` compare; coalescing both would merge two simultaneously-live
    //      values into one register (a miscompile). Loop-φ operands (init vs the
    //      back-edge value) live in different blocks, so they don't interfere and
    //      still coalesce.
    //  (d) φ vs operand, PRECISE: skip an operand whose live range precisely overlaps
    //      the φ's. The live-in/out membership checks (a)/(b) look only at the φ's OWN
    //      block; they miss an operand that interferes with the φ at a PREDECESSOR's
    //      end. E.g. gcd `while(b!=0){t=a%b; a=b; b=t}`: the b-φ's back-edge operand `t`
    //      is computed in the latch, and the b-φ is ALSO live-out of that latch (it's
    //      the a-φ's back-edge operand `a=b`). So `t` and the b-φ both live to the
    //      latch's end and genuinely interfere — coalescing them puts `t` in the b-φ's
    //      register, and the body's `rem` (defining `t`) clobbers `b` before the `a=b`
    //      move reads it. The precise hole-bearing interference catches this; a true
    //      overlap always means coalescing would miscompile, so this never regresses a
    //      correctly-coalesced (byte-identical) loop-φ.
    let mut co = Coalesce::new(nv);
    for v in &f.values {
        if let SsaOp::Phi { operands, .. } = &v.op {
            let b = v.block;
            let mut group: Vec<ValId> = Vec::new();
            for &o in operands {
                if live_in[b].contains(&o) || live_out[b].contains(&o) {
                    continue;
                }
                if ranges_interfere(&ranges, v.id, o) {
                    continue;
                }
                // (c) operand-GROUP interference: `o` may already belong to a coalesce group
                //     pulled together by a DIFFERENT φ. Unioning v.id with o merges that whole
                //     group in, so NO member of o's group may interfere with v.id or with an
                //     operand already coalesced into THIS φ. The earlier check compared only
                //     `o` itself against this φ's operands-so-far; it missed a value pulled in
                //     TRANSITIVELY via o — e.g. running-min `m = x<m ? x : m`, where the loop-φ
                //     `m` coalesces with the ternary φ whose group already holds the load `x`;
                //     x and m are both live at `x<m`, so merging puts two live values in one
                //     register (`if-le v0,v0`, m clobbered). Checking o's group against v.id +
                //     this φ's operands catches that without over-blocking v.id's INHERITED
                //     group members (which would needlessly break legitimate handler/loop-φ
                //     coalescing); the complete post-alloc guard still bails on any residual.
                let lo = co.find(o);
                let bad = (0..nv as u32).filter(|&x| co.find(x) == lo).any(|q| {
                    ranges_interfere(&ranges, v.id, q)
                        || group.iter().any(|&g| ranges_interfere(&ranges, g, q))
                });
                if bad {
                    continue;
                }
                co.union(v.id, o);
                group.push(o);
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

    // 3. Register assignment via PRECISE interference. Each coalescing group's live
    //    range is the union of its members' (hole-bearing) segments; a register is
    //    free for the current group iff no already-assigned group whose segments
    //    OVERLAP this group's holds it. This is what lets a fresh value reuse the
    //    register of an argument/value that is DEAD in the block where the new value
    //    is defined (e.g. `if(x>0)s=1;...` reusing `x`'s register for `s`) — a coarse
    //    [min,max] span would falsely keep that register busy across the hole.
    let _ = intervals;
    let mut group_segs: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
    let mut group_wide: BTreeMap<u32, bool> = BTreeMap::new();
    let mut group_start: BTreeMap<u32, u32> = BTreeMap::new();
    for v in 0..nv as u32 {
        if is_rematerialized(f, v) || ranges[v as usize].is_empty() {
            continue;
        }
        let leader = co.find(v);
        group_segs.entry(leader).or_default().extend_from_slice(&ranges[v as usize]);
        *group_wide.entry(leader).or_insert(false) |= f.values[v as usize].wide;
        let st = ranges[v as usize].iter().map(|s| s.0).min().unwrap();
        group_start.entry(leader).and_modify(|x| *x = (*x).min(st)).or_insert(st);
    }
    let mut order: Vec<u32> = group_segs.keys().copied().collect();
    order.sort_by_key(|&g| (group_start[&g], g));

    let mut max_reg: i32 = num_arg as i32 - 1;
    let mut assigned: Vec<u32> = Vec::new(); // group leaders with a register
    for &g in &order {
        let wide = group_wide[&g];
        let need = if wide { 2 } else { 1 };
        // Pre-colored (argument) groups already have a register.
        if reg[g as usize] != NO_REG {
            max_reg = max_reg.max(reg[g as usize] as i32 + need as i32 - 1);
            assigned.push(g);
            continue;
        }
        // Registers held by already-assigned groups whose ranges overlap g's.
        let mut occupied = vec![false; (max_reg + 3).max(num_arg as i32 + 3) as usize];
        for &h in &assigned {
            if segs_interfere(&group_segs[&g], &group_segs[&h]) {
                let r = reg[h as usize] as usize;
                if r + 1 >= occupied.len() {
                    occupied.resize(r + 2, false);
                }
                occupied[r] = true;
                if group_wide[&h] {
                    occupied[r + 1] = true;
                }
            }
        }
        // d8 reuses an operand's register for a result ONLY when a 2addr form exists; a
        // comparison (cmp-long / cmpl/cmpg-float/double — all 23x, no 2addr form) never
        // does, so its result must NOT land on a (dead) operand's register. Mark the
        // operands occupied so the result takes a fresh register, matching d8. (Additive
        // — can only push the result to a higher register, never a miscompile. Live
        // operands are already occupied, so byte-identical cmps like DblCmp are unchanged.)
        if let SsaOp::Cmp { a, b, .. } = f.values[g as usize].op {
            for o in [a, b] {
                let r = reg[co.find(o) as usize];
                if r != NO_REG {
                    let r = r as usize;
                    if r + 1 >= occupied.len() {
                        occupied.resize(r + 2, false);
                    }
                    occupied[r] = true;
                    if f.values[o as usize].wide {
                        occupied[r + 1] = true;
                    }
                }
            }
        }
        let fits = |r: usize, occ: &[bool]| -> bool {
            let straddle = wide && num_arg > 0 && r == (num_arg as usize - 1);
            !straddle && (0..need).all(|k| r + k >= occ.len() || !occ[r + k])
        };
        // d8 hints a binop/unop RESULT to a dying operand's register (the 2addr form
        // `add-int/2addr vA,vB` reuses the left operand vA; a commutative op may reuse
        // the right). A dead operand doesn't interfere with the result, so its
        // register is free here — prefer it over the lowest free one to match d8.
        let cands: Vec<ValId> = match &f.values[g as usize].op {
            SsaOp::Binop { a, b, jvm_op, .. } => {
                if crate::bootstrap::is_commutative(*jvm_op) {
                    vec![*a, *b]
                } else {
                    vec![*a]
                }
            }
            SsaOp::Unop { a, .. } => vec![*a],
            _ => Vec::new(),
        };
        let preferred = cands.iter().find_map(|&o| {
            let r = reg[co.find(o) as usize];
            (r != NO_REG && fits(r as usize, &occupied)).then_some(r as usize)
        });
        let r = preferred.unwrap_or_else(|| {
            // Lowest free allocated register (a pair if wide), not straddling args.
            let mut r = 0usize;
            loop {
                if r + need > occupied.len() {
                    occupied.resize(r + need, false);
                }
                if fits(r, &occupied) {
                    break;
                }
                r += 1;
            }
            r
        });
        reg[g as usize] = r as u16;
        max_reg = max_reg.max((r + need - 1) as i32);
        assigned.push(g);
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
    exceptions: &[ExceptionEntry],
) -> Result<CodeItem> {
    let mut f = build_ssa(cf, bc, params, instance, exceptions)?;
    // `bastore`/`baload` are shared by byte[] AND boolean[]; pick the right DEX variant
    // from the array's component type (the JVM op alone is ambiguous). Runs first so the
    // corrected dex_op flows into cse_loads' value-numbering keys.
    fix_byte_boolean_array_ops(&mut f, params, instance)?;
    // Algebraic identity folding, like d8: `x+0`, `x-0`, `x|0`, `x^0`, `x<<0` → `x`,
    // `x*1` → `x` (the const operand is then dead and DCE removes it). Rewrites uses
    // of the binop to the surviving operand.
    constant_fold(&mut f);
    // Combine chained int add/sub-by-constant (`(i+7)-1` → `i+6`), like d8, when the
    // intermediate is single-use.
    combine_const_adds(&mut f);
    // Block-local redundant-load elimination (LVN), like d8: a repeated `aget`/`iget`/
    // `sget`/`array-length` with the same operands and no intervening store/call reads
    // the same value (`a[i]*a[i]`, `this.x*this.x`) — replaced by the first load.
    cse_loads(&mut f);
    // Dead-code elimination, like d8: a pure value (no side effect, can't throw)
    // with no remaining users is removed — e.g. an `int s = 0` init that every path
    // overwrites before reading, and any φ that only fed it.
    dce(&mut f);
    // Drop a try region that guards NO throwing SSA instruction. The CFG creates an exceptional
    // edge for every raw `is_throwing_op` byte (which counts ALL `ldc`, incl. non-string/-class
    // constants that lower to a non-throwing `const*`), so a try/finally — or synchronized — whose
    // body only loads such a constant has a region but no real throwing op. Its exceptional handler
    // can never fire, so we drop the region (no try_item); `number()` then excludes the
    // now-unreachable handler block from the layout (build_ssa already dropped the no-exceptional-
    // predecessor regions; this also covers the spurious-edge case). Without this the emit's
    // "no guarded throwing instruction" net would bail.
    if !f.exc_regions.is_empty() {
        let keep: Vec<bool> = f
            .exc_regions
            .iter()
            .map(|r| {
                f.values.iter().any(|v| {
                    ssa_op_can_throw(&v.op)
                        && op_jvm_pc(&v.op)
                            .map_or(false, |pc| (r.start_pc..r.end_pc).contains(&(pc as usize)))
                })
            })
            .collect();
        let mut i = 0usize;
        f.exc_regions.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }
    // d8 SINKS a partially-dead initializer (`int r = 0; if (c) r = …; return r;`) into
    // the branch where it survives. We DON'T sink — we materialize the const before the
    // `if` and let it flow via its register / a φ-move on the merge edge (now that
    // φ-moves on branching edges are emitted). That's functionally correct, just not
    // d8's byte-identical sunk shape; we've relaxed byte-identity for coverage.
    // A φ whose operand is a SIBLING φ in the same block is a parallel copy at the loop
    // back-edge (`a = b; b = a` swap, 3-way rotation, sliding windows). A sibling φ updated
    // in place would make the move reading it run AFTER it was overwritten — a lost copy.
    // This is now handled: the back-edge's φ-moves are emitted as ONE set through a single
    // emit_move_list call (at the latch end, or the If's inline/trampoline path), which
    // SEQUENTIALIZES the parallel copy — dependency-ordering chains and breaking cycles with
    // the `registers_used` scratch temp. (emit_move_list still bails — never miscompiles —
    // on the residue it can't sequentialize: wide cycles, scratch ≥ 16.)
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
    let mut alloc = allocate(&f, &num, &ivs);
    // Safety net against OVER-COALESCING (never miscompile): if ANY instruction reads two
    // DISTINCT value-operands that landed in the SAME register, the coalescer merged two
    // simultaneously-live values (e.g. `m = x>m ? x : m` coalescing x with m → the compare
    // becomes `if-le v0,v0`; or `f(a,b)` becoming `invoke {v0,v0}`). That conflates the
    // operands — bail rather than emit wrong code. This checks EVERY operand-bearing op
    // (binops/cmps AND invokes/array/field/... via `operands`, plus branch terminators via
    // `term_operands`); the earlier guard only covered binop/cmp/branch, so an over-coalesced
    // invoke/array/field operand pair would have miscompiled SILENTLY. Precise (no false
    // positives): `a op a` is one value used twice (a==b, never flagged), and two DISTINCT
    // values both live at one instruction always interfere, so a correct allocation never
    // shares their register. (A proper fix would make the coalescer's interference precise so
    // these methods dex instead of bail; until then this is a complete never-miscompile net.)
    let conflated = |a: ValId, b: ValId| a != b && alloc.reg[a as usize] == alloc.reg[b as usize];
    let any_conflated = |ops: &[ValId]| -> bool {
        ops.iter().enumerate().any(|(i, &a)| ops[i + 1..].iter().any(|&b| conflated(a, b)))
    };
    for v in &f.values {
        if any_conflated(&operands(&v.op)) {
            bail!("ssa: over-coalesce conflated two live operands into one register");
        }
    }
    for blk in &f.blocks {
        if any_conflated(&term_operands(&blk.term)) {
            bail!("ssa: over-coalesce conflated two live branch operands into one register");
        }
    }
    // Multi-predecessor handler safety (never miscompile): a handler-φ snapshots a slot's
    // version at EACH throw point in the guarded region. Exceptional edges can't carry
    // φ-moves (the exception transfers control abruptly), so EVERY operand must already
    // share the φ's register — the slot's value is then in that register whichever throw
    // fires. If the coalescer couldn't unify them all, BAIL (a move on the exceptional
    // edge is impossible) rather than let the handler read a stale register.
    for hb in 0..f.blocks.len() {
        if f.caught[hb].is_none() {
            continue; // not an exception handler block
        }
        for &phi in &f.blocks[hb].phis {
            if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
                let r = alloc.reg[phi as usize];
                if operands.iter().any(|&o| alloc.reg[o as usize] != r) {
                    bail!("ssa: multi-pred handler-φ operands not coalesced into one register (no move on an exceptional edge)");
                }
            }
        }
    }
    // >16-register iget/iput SPILL: iget/iput (22c) has only nibble register fields, so an
    // operand whose FINAL register is ≥16 can't be encoded directly. Reserve 2 low scratch
    // registers and route the high operand(s) through them via move(-object)/from16 in emit_field.
    // Object operands move via move-object/from16 (0x08); only non-wide field accesses qualify.
    let num_arg = f.num_arg_registers;
    let mut spill_base: Option<u16> = None;
    let est = alloc.registers_used.max(num_arg);
    // A switch lowers to a `const tmp,k; if-eq key,tmp` chain whose temp sits at
    // `registers_used`; once that's ≥16 the nibble `if-eq` can't hold it, and (unlike the field/
    // invoke cases) this is knowable PRE-emit (it doesn't depend on scratch), so reserve here so
    // the Switch terminator routes the temp (and a high key) through the 2 low scratch.
    let switch_needs_low_tmp = alloc.registers_used >= 16
        && f.blocks.iter().any(|b| matches!(b.term, Terminator::Switch { .. }));
    if (est > 16 || switch_needs_low_tmp) && num_arg <= 14 {
        let high = |r: u16| r != NO_REG && crate::regalloc::remap_register(r, num_arg, est) >= 16;
        let needs_spill = switch_needs_low_tmp
            || f.values.iter().enumerate().any(|(i, val)| match &val.op {
                SsaOp::GetField { dex_op, obj, .. } if *dex_op != 0x53 => {
                    high(alloc.reg[i]) || high(alloc.reg[*obj as usize])
                }
                SsaOp::PutField { dex_op, obj, value, .. }
                    if *dex_op != 0x5a && !f.values[*value as usize].wide =>
                {
                    high(alloc.reg[*value as usize]) || high(alloc.reg[*obj as usize])
                }
                _ => false,
            });
        if needs_spill {
            reserve_scratch(&mut alloc, num_arg, 2);
            spill_base = Some(num_arg);
        }
    }
    build_dex(&f, &num, &alloc, line_numbers, params, spill_base, None)
}

/// Rewrites every operand ValId of `op` through `g` (for value substitution).
fn map_operands(op: &mut SsaOp, mut g: impl FnMut(ValId) -> ValId) {
    match op {
        SsaOp::Phi { operands, .. } => {
            for o in operands {
                *o = g(*o);
            }
        }
        SsaOp::Binop { a, b, .. } | SsaOp::Cmp { a, b, .. } => {
            *a = g(*a);
            *b = g(*b);
        }
        SsaOp::Unop { a, .. } => *a = g(*a),
        SsaOp::Invoke { args, .. } => {
            for o in args {
                *o = g(*o);
            }
        }
        SsaOp::GetField { obj, .. } => *obj = g(*obj),
        SsaOp::PutStatic { value, .. } => *value = g(*value),
        SsaOp::PutField { obj, value, .. } => {
            *obj = g(*obj);
            *value = g(*value);
        }
        SsaOp::ArrayGet { array, index, .. } => {
            *array = g(*array);
            *index = g(*index);
        }
        SsaOp::ArrayPut { array, index, value, .. } => {
            *array = g(*array);
            *index = g(*index);
            *value = g(*value);
        }
        SsaOp::ArrayLength { array, .. } => *array = g(*array),
        SsaOp::NewArray { size, .. } => *size = g(*size),
        SsaOp::Monitor { obj, .. } => *obj = g(*obj),
        SsaOp::CheckCast { obj, .. } | SsaOp::InstanceOf { obj, .. } => *obj = g(*obj),
        SsaOp::Argument { .. }
        | SsaOp::ConstInt(_)
        | SsaOp::ConstLong(_)
        | SsaOp::ConstString { .. }
        | SsaOp::ConstClass { .. }
        | SsaOp::GetStatic { .. }
        | SsaOp::NewInstance { .. }
        | SsaOp::CaughtException => {}
    }
}

/// Algebraic-identity constant folding (matches d8). A binop with a constant
/// identity operand is replaced by its other operand: `x+0`/`0+x`, `x-0`, `x|0`/
/// `0|x`, `x^0`/`0^x`, `x<<0`/`x>>0`/`x>>>0` → `x`; `x*1`/`1*x` → `x`. (div/rem are
/// throwing and left alone.) Uses are rewritten to the surviving operand; the now-
/// dead binop and constant are swept by `dce`.
/// Combines a chained int add/sub by constants into one, matching d8: `(y±c1)±c2` →
/// `y + (±c1±c2)` (`(i+7)-1` → `i+6`). Only when the intermediate `y±c1` is SINGLE-USE
/// (so it's eliminated, not recomputed — d8 keeps it if shared). The rewritten op uses a
/// fresh combined ConstInt; the dead intermediate + old consts are swept by dce.
fn combine_const_adds(f: &mut SsaFn) {
    let n = f.values.len();
    // Use count of each value across all operands + terminators.
    let mut uses = vec![0u32; n];
    for u in &f.values {
        match &u.op {
            SsaOp::Phi { operands, .. } => {
                for &o in operands {
                    uses[o as usize] += 1;
                }
            }
            other => {
                for o in operands(other) {
                    uses[o as usize] += 1;
                }
            }
        }
    }
    for b in &f.blocks {
        for o in term_operands(&b.term) {
            uses[o as usize] += 1;
        }
    }
    let cint = |f: &SsaFn, x: ValId| match f.values[x as usize].op {
        SsaOp::ConstInt(c) => Some(c),
        _ => None,
    };
    // (V, y, jvm_pc, combined-const)
    let mut rewrites: Vec<(usize, ValId, u32, i32)> = Vec::new();
    for v in 0..n {
        let (op2, x, b2, pc2) = match f.values[v].op {
            SsaOp::Binop { jvm_op, a, b, jvm_pc } => (jvm_op, a, b, jvm_pc),
            _ => continue,
        };
        if !matches!(op2, 0x60 | 0x64) {
            continue; // iadd / isub only (int)
        }
        let c2 = match cint(f, b2) {
            Some(c) => c,
            None => continue, // c2 must be a constant (right operand)
        };
        if uses[x as usize] != 1 {
            continue; // intermediate must be single-use (else d8 keeps it)
        }
        let (op1, y, b1) = match f.values[x as usize].op {
            SsaOp::Binop { jvm_op, a, b, .. } => (jvm_op, a, b),
            _ => continue,
        };
        if !matches!(op1, 0x60 | 0x64) {
            continue;
        }
        let c1 = match cint(f, b1) {
            Some(c) => c,
            None => continue,
        };
        let d1 = if op1 == 0x60 { c1 } else { c1.wrapping_neg() };
        let d2 = if op2 == 0x60 { c2 } else { c2.wrapping_neg() };
        rewrites.push((v, y, pc2, d1.wrapping_add(d2)));
    }
    if rewrites.is_empty() {
        return;
    }
    let base = f.values.len() as ValId;
    for (i, &(_, _, _, combined)) in rewrites.iter().enumerate() {
        let id = base + i as u32;
        f.values.push(SsaValue { id, op: SsaOp::ConstInt(combined), wide: false, is_ref: false, block: 0 });
    }
    for (i, &(v, y, pc, _)) in rewrites.iter().enumerate() {
        f.values[v].op = SsaOp::Binop { jvm_op: 0x60, a: y, b: base + i as u32, jvm_pc: pc };
    }
}

fn constant_fold(f: &mut SsaFn) {
    let n = f.values.len();
    let cv = |f: &SsaFn, x: ValId| -> Option<i64> {
        match &f.values[x as usize].op {
            SsaOp::ConstInt(c) => Some(*c as i64),
            SsaOp::ConstLong(c) => Some(*c),
            _ => None,
        }
    };
    let mut repl: Vec<ValId> = (0..n as ValId).collect();
    for v in 0..n {
        if let SsaOp::Binop { jvm_op, a, b, .. } = f.values[v].op {
            let (lc, rc) = (cv(f, a), cv(f, b));
            let surviving = match jvm_op {
                // add / or / xor — identity 0, either side.
                0x60 | 0x61 | 0x80 | 0x81 | 0x82 | 0x83 => {
                    if rc == Some(0) {
                        Some(a)
                    } else if lc == Some(0) {
                        Some(b)
                    } else {
                        None
                    }
                }
                // sub — identity 0 on the right (0-x ≠ x).
                0x64 | 0x65 => (rc == Some(0)).then_some(a),
                // mul — identity 1, either side.
                0x68 | 0x69 => {
                    if rc == Some(1) {
                        Some(a)
                    } else if lc == Some(1) {
                        Some(b)
                    } else {
                        None
                    }
                }
                // shifts (i/l shl, shr, ushr) — shift by 0 on the right.
                0x78..=0x7d => (rc == Some(0)).then_some(a),
                _ => None,
            };
            if let Some(s) = surviving {
                repl[v] = s;
            }
        }
    }
    fn find(repl: &[ValId], mut x: ValId) -> ValId {
        while repl[x as usize] != x {
            x = repl[x as usize];
        }
        x
    }
    if repl.iter().enumerate().all(|(i, &r)| i as ValId == r) {
        return; // nothing folded
    }
    for v in 0..n {
        let r = &repl;
        map_operands(&mut f.values[v].op, |x| find(r, x));
    }
    for b in &mut f.blocks {
        match &mut b.term {
            Terminator::If { operands, .. } => {
                for o in operands {
                    *o = find(&repl, *o);
                }
            }
            Terminator::Return { value: Some(v), .. } => *v = find(&repl, *v),
            Terminator::Throw { value, .. } => *value = find(&repl, *value),
            Terminator::Switch { value, .. } => *value = find(&repl, *value),
            _ => {}
        }
    }
}

/// `bastore`/`baload` (JVM) are SHARED by `byte[]` and `boolean[]`; DEX splits them by
/// the array's component type — aput-byte/aget-byte (0x4f/0x48) for byte[],
/// aput-boolean/aget-boolean (0x4e/0x47) for boolean[]. The SSA builder defaults to the
/// byte variant (it only sees the JVM op). Here we trace each such op's array operand to
/// its declared element type and rewrite to the boolean variant for a `boolean[]`. Using
/// the byte op on a `boolean[]` is an ART VerifyError, so when the element type can't be
/// determined we BAIL rather than risk a miscompile. (The boolean op is exactly one
/// opcode below its byte counterpart, hence `dex_op - 1`.)
fn fix_byte_boolean_array_ops(f: &mut SsaFn, params: &[String], instance: bool) -> Result<()> {
    // The descriptor of the array a value holds, if statically determinable.
    fn array_desc(
        f: &SsaFn,
        params: &[String],
        instance: bool,
        v: ValId,
        depth: u32,
    ) -> Option<String> {
        if depth > 32 {
            return None;
        }
        match &f.values[v as usize].op {
            SsaOp::Argument { index } => {
                let pi = if instance { index.checked_sub(1)? } else { *index };
                params.get(pi).cloned()
            }
            SsaOp::NewArray { type_desc, .. } => Some(type_desc.clone()),
            SsaOp::GetField { field, .. } | SsaOp::GetStatic { field, .. } => {
                Some(field.type_.clone())
            }
            SsaOp::Invoke { method, .. } => Some(method.proto.return_type.clone()),
            // element of an array-of-arrays: "[[Z" → "[Z".
            SsaOp::ArrayGet { array, .. } => {
                array_desc(f, params, instance, *array, depth + 1)?.strip_prefix('[').map(str::to_string)
            }
            // all φ operands must agree on a descriptor.
            SsaOp::Phi { operands, .. } => {
                let mut desc: Option<String> = None;
                for &o in operands.iter().filter(|&&o| o != v) {
                    let d = array_desc(f, params, instance, o, depth + 1)?;
                    match &desc {
                        None => desc = Some(d),
                        Some(prev) if *prev == d => {}
                        Some(_) => return None,
                    }
                }
                desc
            }
            _ => None,
        }
    }
    let mut rewrites: Vec<(usize, u16)> = Vec::new();
    for v in 0..f.values.len() {
        let (dex_op, array) = match &f.values[v].op {
            SsaOp::ArrayGet { dex_op, array, .. } if *dex_op == 0x48 => (*dex_op, *array),
            SsaOp::ArrayPut { dex_op, array, .. } if *dex_op == 0x4f => (*dex_op, *array),
            _ => continue,
        };
        match array_desc(f, params, instance, array, 0).as_deref() {
            Some("[Z") => rewrites.push((v, dex_op - 1)), // boolean[] → boolean variant
            Some("[B") => {}                              // byte[] → keep byte variant
            _ => bail!("ssa: bastore/baload on an array of undetermined byte-vs-boolean type"),
        }
    }
    for (v, op) in rewrites {
        match &mut f.values[v].op {
            SsaOp::ArrayGet { dex_op, .. } | SsaOp::ArrayPut { dex_op, .. } => *dex_op = op,
            _ => unreachable!(),
        }
    }
    Ok(())
}

/// Block-local memory value-numbering, matching d8 — two complementary rewrites over a
/// single block's `aget`/`iget`/`sget`/`array-length` reads:
///   • REDUNDANT-LOAD CSE: a second read with the SAME operands as an earlier read,
///     with no intervening store/call, yields the identical value and throws under
///     identical conditions (the earlier read dominates it within the block) — so it's
///     replaced by the earlier read.
///   • STORE-TO-LOAD FORWARDING: a read of a location that was just STORED, with no
///     intervening store/call, yields the stored value (`a[i]=v; …a[i]` → v;
///     `this.x=v; …this.x` → v) — so it's replaced by the stored operand. d8 forwards
///     the stored operand directly even for narrowing element types (`byte[]`): valid
///     bytecode normalizes the value (`int-to-byte`) BEFORE the store, so it already
///     equals what an `aget-byte` would read back. The matching load op is always the
///     store op minus 7 (aput→aget / iput→iget / sput→sget).
/// Loads are `can_throw`, so `dce` keeps a dead replaced load; this pass removes it from
/// the block body directly. Invalidation is maximally conservative: ANY store or call
/// clears the whole cache (could alias any array/field). Block-local only, so the
/// earlier read/store always dominates the later read — no cross-block reasoning.
fn cse_loads(f: &mut SsaFn) {
    #[derive(PartialEq, Clone)]
    enum Key {
        Arr(u16, ValId, ValId),
        Field(u16, ValId, FieldRef),
        Static(u16, FieldRef),
        Len(ValId),
    }
    fn load_key(op: &SsaOp) -> Option<Key> {
        match op {
            SsaOp::ArrayGet { dex_op, array, index, .. } => Some(Key::Arr(*dex_op, *array, *index)),
            SsaOp::GetField { dex_op, field, obj, .. } => {
                Some(Key::Field(*dex_op, *obj, field.clone()))
            }
            SsaOp::GetStatic { dex_op, field, .. } => Some(Key::Static(*dex_op, field.clone())),
            SsaOp::ArrayLength { array, .. } => Some(Key::Len(*array)),
            _ => None,
        }
    }
    // Process one block's body, threading the available-loads/forwards cache `avail`.
    // When `record`, rewrites redundant loads (repl/removed); otherwise it only updates
    // `avail` (to compute avail_out for the cross-block step). A store clears the cache
    // (any aliasing) then publishes the stored value (forwarding); a call clears it.
    fn cse_block(
        f: &SsaFn,
        b: usize,
        avail: &mut Vec<(Key, ValId)>,
        repl: &mut [ValId],
        removed: &mut [bool],
        record: bool,
    ) {
        for &v in &f.blocks[b].body {
            match &f.values[v as usize].op {
                SsaOp::ArrayPut { dex_op, array, index, value, .. } => {
                    let k = Key::Arr(*dex_op - 7, *array, *index);
                    avail.clear();
                    avail.push((k, *value));
                }
                SsaOp::PutField { dex_op, field, obj, value, .. } => {
                    let k = Key::Field(*dex_op - 7, *obj, field.clone());
                    avail.clear();
                    avail.push((k, *value));
                }
                SsaOp::PutStatic { dex_op, field, value, .. } => {
                    let k = Key::Static(*dex_op - 7, field.clone());
                    avail.clear();
                    avail.push((k, *value));
                }
                SsaOp::Invoke { .. } => avail.clear(),
                op => {
                    if let Some(k) = load_key(op) {
                        if let Some(&(_, first)) = avail.iter().find(|(ek, _)| *ek == k) {
                            if record {
                                repl[v as usize] = first;
                                removed[v as usize] = true;
                            }
                        } else {
                            avail.push((k, v));
                        }
                    }
                }
            }
        }
    }
    let n = f.values.len();
    let nb = f.blocks.len();
    let mut repl: Vec<ValId> = (0..n as ValId).collect();
    let mut removed = vec![false; n];
    // BLOCK-LOCAL only: each block starts with an empty cache. (Cross-block CSE was
    // attempted but reverted — extending a load's live range across blocks lets the
    // φ-coalescer wrongly merge it with an INTERFERING value: `m = a[i]>m ? a[i] : m`
    // coalesced a[i] into m's register and clobbered m before the compare. The
    // over-coalescing is the real bug to fix before cross-block CSE — see memory.)
    for b in 0..nb {
        let mut avail: Vec<(Key, ValId)> = Vec::new();
        cse_block(f, b, &mut avail, &mut repl, &mut removed, true);
    }
    if !removed.iter().any(|&r| r) {
        return; // nothing redundant
    }
    fn find(repl: &[ValId], mut x: ValId) -> ValId {
        while repl[x as usize] != x {
            x = repl[x as usize];
        }
        x
    }
    for v in 0..n {
        let r = &repl;
        map_operands(&mut f.values[v].op, |x| find(r, x));
    }
    for blk in &mut f.blocks {
        match &mut blk.term {
            Terminator::If { operands, .. } => {
                for o in operands {
                    *o = find(&repl, *o);
                }
            }
            Terminator::Return { value: Some(v), .. } => *v = find(&repl, *v),
            Terminator::Throw { value, .. } => *value = find(&repl, *value),
            Terminator::Switch { value, .. } => *value = find(&repl, *value),
            _ => {}
        }
        blk.body.retain(|&v| !removed[v as usize]);
    }
}

/// Dead-code elimination over the SSA value graph (matches d8's): a value is LIVE
/// if it can throw / has a side effect (`ssa_op_can_throw` — calls, stores, field/
/// array access, div/rem, new, const-string), or is used by a terminator, or is an
/// operand of another live value. Everything else (pure, unused — a const, φ,
/// arithmetic, conversion, comparison) is removed from its block's body/φ list.
/// Iterated to a fixpoint so a dead φ feeding only another dead value also goes.
fn dce(f: &mut SsaFn) {
    let nv = f.values.len();
    let mut live = vec![false; nv];
    let mut work: Vec<ValId> = Vec::new();
    let mark = |v: ValId, live: &mut Vec<bool>, work: &mut Vec<ValId>| {
        if !live[v as usize] {
            live[v as usize] = true;
            work.push(v);
        }
    };
    for b in 0..f.blocks.len() {
        for &v in &f.blocks[b].body {
            if ssa_op_can_throw(&f.values[v as usize].op) {
                mark(v, &mut live, &mut work);
            }
        }
        for u in term_operands(&f.blocks[b].term) {
            mark(u, &mut live, &mut work);
        }
    }
    while let Some(v) = work.pop() {
        let opnds: Vec<ValId> = match &f.values[v as usize].op {
            SsaOp::Phi { operands, .. } => operands.clone(),
            other => operands(other),
        };
        for o in opnds {
            mark(o, &mut live, &mut work);
        }
    }
    for b in 0..f.blocks.len() {
        f.blocks[b].body.retain(|&v| live[v as usize]);
        f.blocks[b].phis.retain(|&v| live[v as usize]);
    }
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

/// The (kind, value) of a constant, for identity comparison; `None` if not a
/// numeric constant. Two loop-var inits with the same key are GVN-identical in d8.
fn const_key(op: &SsaOp) -> Option<(u8, i64)> {
    match op {
        SsaOp::ConstInt(c) => Some((0, *c as i64)),
        SsaOp::ConstLong(c) => Some((1, *c)),
        _ => None,
    }
}

/// Reorders, within each block, the constant entry-initializers that feed loop
/// φ-nodes into φ first-use order (counter before accumulator) — but ONLY when all
/// those inits are GVN-IDENTICAL constants. d8 shares identical init constants
/// (removing them from the φ coalesce groups), which flips the loop-variable
/// register order to first-read (counter gets v0). When the inits differ (e.g.
/// `s=0, i=1`, or a wide `0L` accumulator vs an int `0` counter), d8 keeps
/// source/bytecode order (the accumulator, declared first, gets the low register),
/// so we leave the block body untouched. Only permutes pure constants among the
/// positions they already occupy, never crossing a data dependency.
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
        // d8 shares GVN-identical init constants, so φs initialized by the SAME const
        // are ordered among themselves by FIRST READ (counter before accumulator),
        // while distinct-const groups keep their source order. Generalize: group the
        // const-inits by value, order the groups by first source occurrence, and order
        // within each group by φ first-use rank. Only applies when ALL reordered inits
        // are constants (else d8's scheduling is murkier — leave source order).
        let body = &mut f.blocks[b].body;
        let positions: Vec<usize> =
            body.iter().enumerate().filter(|(_, &v)| init_rank.contains_key(&v)).map(|(i, _)| i).collect();
        let mut items: Vec<ValId> = positions.iter().map(|&i| body[i]).collect();
        if items.iter().any(|&v| const_key(&f.values[v as usize].op).is_none()) {
            continue;
        }
        // Anchor (source position) of each const group = the lowest item index it holds.
        let mut group_anchor: BTreeMap<(u8, i64), usize> = BTreeMap::new();
        for (i, &v) in items.iter().enumerate() {
            let k = const_key(&f.values[v as usize].op).unwrap();
            group_anchor.entry(k).and_modify(|x| *x = (*x).min(i)).or_insert(i);
        }
        items.sort_by_key(|&v| {
            let k = const_key(&f.values[v as usize].op).unwrap();
            (group_anchor[&k], init_rank[&v], v)
        });
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
    spill_base: Option<u16>,
    // The true frame size (`registers_size`, incl. scratch) for `emit_field`'s high-operand
    // test. Set on the spill RE-EMIT pass (below) so an arg whose real register is pushed ≥16
    // by scratch inflation is detected; `None` on the first pass (uses `registers_used`). The
    // remap is frame-independent for locals, so this only matters for argument operands.
    frame_hint: Option<u16>,
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
    // A switch is lowered to a `const tmp,k; if-eq key,tmp,case` chain needing ONE
    // scratch register, reserved just above the allocated set (it remaps cleanly below
    // the args and is live only transiently within the chain). registers_size is bumped
    // by 1 below iff a switch actually used it.
    let switch_scratch = alloc.registers_used;
    let mut used_switch_scratch = false;
    // Critical taken-edge trampolines: (target block, φ-moves). Each becomes a synthetic
    // block (id = f.blocks.len() + index) emitted after all code — `moves; goto target` —
    // and the critical `if`'s taken branch is redirected to it (edge-splitting on the
    // taken side; the fall-through side is handled inline in the If terminator).
    let mut trampolines: Vec<(usize, Vec<(ValId, ValId)>)> = Vec::new();
    // Range-invoke arg-marshalling block: also reserved just above the allocated set
    // (`alloc.registers_used`). Tracks the LARGEST block any range-invoke needed; the
    // block is transient per call, so calls reuse it (and it temporally never overlaps
    // the 1-reg switch scratch at the same base). registers_size is bumped to cover it.
    let mut range_block_words: u16 = 0;
    // φ-move cycle-breaking scratch (parallel-copy sequentialization): a swap needs ONE temp
    // at `registers_used` (the guaranteed-dead high-water mark, same base as switch_scratch /
    // range_block — temporally disjoint, so they max rather than sum). Bumped iff a cycle hit.
    let mut phi_scratch_words: u16 = 0;

    // Return tail-duplication (d8's): a goto/fall predecessor of a TRIVIAL return
    // block (empty body, `return v`/`return-void`, not an exception handler) inlines
    // that return — `return <the φ-operand for this edge>` — instead of branching to
    // it, in either of two cases:
    //   • the return block has NO `if`-predecessor and this pred isn't the one laid
    //     out immediately before it (so inlining drops a `goto`); the adjacent pred
    //     falls through and the block emits its return once (deadCatch), OR
    //   • this edge carries a pending φ-move (the returned value's φ-operand for this
    //     edge isn't in the φ's register) — inlining `return operand` ABSORBS the
    //     move (clamp/max3's `r=hi; return r` → `return hi`), even when the block has
    //     other (`if`) predecessors.
    // A return block reached by a kept `if`/goto edge stays a real block (loops are
    // unaffected — their exit return is the loop condition's `if` target).
    let n = f.blocks.len();
    let is_tret = |blk: usize| {
        f.blocks[blk].body.is_empty()
            && matches!(f.blocks[blk].term, Terminator::Return { .. })
            && f.caught[blk].is_none()
    };
    let no_if_pred = |t: usize| {
        f.blocks[t].preds.iter().all(|&p| !matches!(f.blocks[p].term, Terminator::If { .. }))
    };
    // The value returned on edge P→T (φ resolved per-edge) and whether that differs
    // from the φ's own register (a φ-move the inline would absorb).
    let edge_return = |t: usize, p: usize| -> (Option<ValId>, u16, bool) {
        match &f.blocks[t].term {
            Terminator::Return { value: None, op } => (None, *op, false),
            Terminator::Return { value: Some(v), op } => {
                if let SsaOp::Phi { operands, .. } = &f.values[*v as usize].op {
                    // A φ/pred bookkeeping mismatch (seen on some real-world bytecode)
                    // must DEGRADE, not panic: treat the edge as carrying no absorbable
                    // move (no tail-dup benefit) rather than index out of bounds.
                    match f.blocks[t].preds.iter().position(|&x| x == p).and_then(|pi| operands.get(pi).copied()) {
                        Some(o) => (Some(o), *op, alloc.reg[o as usize] != alloc.reg[*v as usize]),
                        None => (Some(*v), *op, false),
                    }
                } else {
                    (Some(*v), *op, false)
                }
            }
            _ => unreachable!("is_tret guarantees a Return terminator"),
        }
    };
    let mut inline_ret = vec![false; n];
    for (pos, &b) in num.layout.iter().enumerate() {
        if let Terminator::Goto { target } | Terminator::Fall { target } = f.blocks[b].term {
            if is_tret(target) {
                let adjacent = num.layout.get(pos + 1).copied() == Some(target);
                let (.., has_move) = edge_return(target, b);
                if (no_if_pred(target) && !adjacent) || has_move {
                    inline_ret[b] = true;
                }
            }
        }
    }
    // A trivial return block is still emitted iff some predecessor reaches it without
    // inlining (an `if` edge, or a goto/fall that kept its branch / fell through).
    let mut tret_emitted = vec![false; n];
    for t in 0..n {
        if !is_tret(t) {
            continue;
        }
        tret_emitted[t] = f.blocks[t].preds.iter().any(|&p| match &f.blocks[p].term {
            Terminator::Goto { target } | Terminator::Fall { target } if *target == t => !inline_ret[p],
            _ => true,
        });
    }

    // (jvm_pc, dex_start, dex_end) of each emitted throwing instruction — used to
    // narrow each try_item to its guarded DEX address range.
    let mut throw_spans: Vec<(u32, usize, usize)> = Vec::new();
    for (pos, &b) in num.layout.iter().enumerate() {
        block_unit[b] = insns.len();
        // φ-moves for an incoming edge from a BRANCHING single-... predecessor land here,
        // at this block's entry (before the body), since they can't sit at the branching
        // pred's end. block_unit[b] points at them so the branch lands here first.
        emit_entry_phi_moves(f, &mut insns, alloc, b, &mut phi_scratch_words, frame_hint)?;
        // Wide-const sharing (d8): within a block, a wide const equal to one still held
        // live in a register is copied via `move-wide` (1 word) instead of re-materialized
        // (`const-wide*` ≥2 words) — e.g. `long i=0, s=0` → `const-wide v0; move-wide v2,v0`.
        // (value, register) pairs currently known to hold a wide const. Reset per block
        // (we only share within a block, so the source always still holds the value).
        let mut wide_consts: Vec<(i64, u16)> = Vec::new();
        for &v in &f.blocks[b].body {
            if is_rematerialized(f, v) {
                continue;
            }
            let dex_start = insns.len();
            // Maintain the wide-const tracker: a non-const op may clobber any register, so
            // clear it; a narrow const clobbers only its own register.
            match &f.values[v as usize].op {
                SsaOp::ConstLong(_) => {}
                SsaOp::ConstInt(_) => {
                    let r = reg(v);
                    wide_consts.retain(|&(_, s)| s != r && s + 1 != r);
                }
                _ => wide_consts.clear(),
            }
            if let SsaOp::ConstLong(c) = f.values[v as usize].op {
                let r = reg(v);
                // a wide write to [r,r+1] invalidates any tracked pair within one register.
                wide_consts.retain(|&(_, s)| (s as i32 - r as i32).abs() > 1);
                // Share via move-wide (12x) when both regs fit a nibble, copying from the
                // MOST-RECENTLY materialized same-value reg (d8 chains: a→b→c, each copies
                // from the previous, not all from the first).
                let src = if r <= 15 {
                    wide_consts.iter().rev().find(|&&(val, s)| val == c && s <= 15).map(|&(_, s)| s)
                } else {
                    None
                };
                match src {
                    Some(s) => insns.push(0x04 | (r << 8) | (s << 12)), // move-wide r, s
                    None => emit_const_long(&mut insns, r, c),
                }
                wide_consts.push((c, r));
                continue;
            }
            match &f.values[v as usize].op {
                SsaOp::Invoke { .. } => {
                    emit_invoke(f, &mut insns, alloc, v, &mut pool_fixups, &mut outs, &mut positions, line_numbers, &mut range_block_words, frame_hint)?;
                }
                SsaOp::GetField { .. } | SsaOp::GetStatic { .. } | SsaOp::PutField { .. } | SsaOp::PutStatic { .. } => {
                    emit_field(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers, spill_base, frame_hint)?;
                }
                SsaOp::ArrayGet { .. } | SsaOp::ArrayPut { .. } | SsaOp::ArrayLength { .. } => {
                    emit_array(f, &mut insns, alloc, v, &mut positions, line_numbers, spill_base, frame_hint)?;
                }
                SsaOp::NewInstance { .. } | SsaOp::NewArray { .. } => {
                    emit_alloc(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers)?;
                }
                SsaOp::ConstString { .. } => {
                    emit_const_string(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers);
                }
                SsaOp::ConstClass { .. } => {
                    emit_const_class(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers);
                }
                SsaOp::CheckCast { .. } => {
                    emit_check_cast(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers, frame_hint)?;
                }
                SsaOp::InstanceOf { .. } => {
                    emit_instance_of(f, &mut insns, alloc, v, &mut pool_fixups, &mut positions, line_numbers, spill_base, frame_hint)?;
                }
                SsaOp::CaughtException => {
                    // `move-exception dest` (11x) — only reached when the caught value
                    // is read (build_ssa keeps unused ones out of the block body).
                    insns.push(0x0d | (reg(v) << 8));
                }
                SsaOp::Monitor { enter, obj, jvm_pc } => {
                    // monitor-enter (0x1d) / monitor-exit (0x1e), 11x: AA = objectref reg.
                    // Throwing → record a debug position; the throw_spans coverage below
                    // (via ssa_op_can_throw/op_jvm_pc) extends any enclosing try_item over it.
                    if let Some(line) = crate::bootstrap::line_for(line_numbers, *jvm_pc) {
                        positions.push((insns.len() as u32, line));
                    }
                    let dexop: u16 = if *enter { 0x1d } else { 0x1e };
                    insns.push(dexop | (reg(*obj) << 8));
                }
                _ => {
                    // div/rem are throwing — record a position at the instruction.
                    if let SsaOp::Binop { jvm_op, jvm_pc, .. } = &f.values[v as usize].op {
                        if matches!(jvm_op, 0x6c | 0x6d | 0x70 | 0x71) {
                            if let Some(line) = crate::bootstrap::line_for(line_numbers, *jvm_pc) {
                                positions.push((insns.len() as u32, line));
                            }
                        }
                    }
                    emit_value(f, &mut insns, alloc, v, frame_hint)?;
                }
            }
            // Record the DEX span of a guarded throwing instruction. A call's
            // trailing move-result stays inside the range (d8 keeps invoke+result an
            // atomic unit); only NON-throwing instructions are trimmed.
            if !f.exc_regions.is_empty() && ssa_op_can_throw(&f.values[v as usize].op) {
                if let Some(jpc) = op_jvm_pc(&f.values[v as usize].op) {
                    throw_spans.push((jpc, dex_start, insns.len()));
                }
            }
        }
        // A block that inlines its target's return doesn't branch there, so its
        // successor φ-moves are dead — skip them.
        if !inline_ret[b] {
            // Resolve φ-nodes that didn't coalesce: insert a `move φ ← operand` at the
            // end of this (predecessor) block, before its terminator. Coalesced φs need
            // no move (operand already in the φ's register).
            emit_phi_moves(f, &mut insns, alloc, b, &mut phi_scratch_words, frame_hint)?;
        }
        if inline_ret[b] {
            // Inline the (trivial) return of this block's goto/fall target.
            let target = match &f.blocks[b].term {
                Terminator::Goto { target } | Terminator::Fall { target } => *target,
                _ => unreachable!("inline_ret only set for goto/fall"),
            };
            match &f.blocks[target].term {
                Terminator::Return { value: None, .. } => insns.push(0x0e),
                Terminator::Return { value: Some(v), op } => {
                    // The returned value, as seen on the b→target edge: a φ resolves
                    // to its operand for this predecessor; otherwise the value itself.
                    let operand = if let SsaOp::Phi { operands, .. } = &f.values[*v as usize].op {
                        let pred_idx = f.blocks[target].preds.iter().position(|&p| p == b).unwrap();
                        operands[pred_idx]
                    } else {
                        *v
                    };
                    insns.push(op | (reg(operand) << 8));
                }
                _ => unreachable!("inline_ret target is a trivial return block"),
            }
            continue;
        }
        match &f.blocks[b].term {
            // A trivial return block reached only by inlined edges is skipped.
            Terminator::Return { .. } if is_tret(b) && !tret_emitted[b] => {}
            Terminator::Return { value: None, .. } => insns.push(0x0e),
            Terminator::Return { value: Some(v), op } => {
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
            Terminator::If { jvm_op, operands, taken, fallthrough } => {
                // A TAKEN critical edge (taken block has >1 pred) needing a φ-move routes
                // through a trampoline block (moves + goto taken) emitted after all code;
                // the taken branch is redirected there.
                let taken_target = if f.blocks[*taken].preds.len() > 1 {
                    let moves = phi_moves_for_edge(f, alloc, b, *taken);
                    if moves.is_empty() {
                        *taken
                    } else {
                        let t = f.blocks.len() + trampolines.len();
                        trampolines.push((*taken, moves));
                        t
                    }
                } else {
                    *taken
                };
                let (dexop, two) = crate::bootstrap::cond_branch_dex_op(*jvm_op).unwrap();
                if two {
                    let a = reg(operands[0]);
                    let b2 = reg(operands[1]);
                    // if-test (22t) has no wider form, so a high operand spills through the 2 low
                    // scratch (reserved by the build_dex retry): move it down (move-object/from16
                    // for a reference operand, else move/from16) and compare on the scratch.
                    let (a_use, b_use) = if let Some(sb) = spill_base {
                        let na = f.num_arg_registers;
                        let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
                        let hi = |r: u16| r >= 16 || crate::regalloc::remap_register(r, na, est) >= 16;
                        let au = if hi(a) {
                            emit_copy(&mut insns, sb, a, false, f.values[operands[0] as usize].is_ref, na, est)?;
                            sb
                        } else {
                            a
                        };
                        let bu = if hi(b2) {
                            emit_copy(&mut insns, sb + 1, b2, false, f.values[operands[1] as usize].is_ref, na, est)?;
                            sb + 1
                        } else {
                            b2
                        };
                        (au, bu)
                    } else {
                        (a, b2)
                    };
                    insns.push(dexop | (nib(a_use)? << 8) | (nib(b_use)? << 12));
                } else {
                    insns.push(dexop | (reg(operands[0]) << 8));
                }
                let off = insns.len();
                insns.push(0);
                fixups.push((off, taken_target, false));
                // FALL-THROUGH critical edge: φ-moves emitted HERE (after the `if`, before
                // the adjacent fall-through block) run ONLY on the fall-through path — the
                // taken branch jumped away. Other predecessors of the fall-through block
                // branch to its block_unit (set AFTER these moves), so they skip them.
                let ft_moves = phi_moves_for_edge(f, alloc, b, *fallthrough);
                if !ft_moves.is_empty() && f.blocks[*fallthrough].preds.len() > 1 {
                    emit_move_list(f, &mut insns, alloc, &ft_moves, &mut phi_scratch_words, frame_hint)?;
                }
            }
            Terminator::Throw { value, jvm_pc } => {
                let r = reg(*value);
                if r > 0xff {
                    bail!("ssa dexbuilder: throw operand register > 255 (0x27 is 11x)");
                }
                let dex_start = insns.len();
                insns.push(0x27 | (r << 8));
                // The throw IS the guarded raising instruction — extend its try_item to
                // cover it (else the exception escapes the catch). Mirrors the body-loop
                // throw_spans recording for SsaOp throwing instructions.
                if !f.exc_regions.is_empty() {
                    throw_spans.push((*jvm_pc, dex_start, insns.len()));
                }
            }
            // Switch lowered to a comparison chain: per case `const tmp,k; if-eq key,tmp,
            // case`, then `goto default`. Functional-correct (not d8's packed/sparse-
            // switch payload); reuses the if-eq/const/goto fixup machinery.
            Terminator::Switch { value, default, cases } => {
                let na = f.num_arg_registers;
                let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
                let key = reg(*value);
                let key_hi = key >= 16 || crate::regalloc::remap_register(key, na, est) >= 16;
                // The if-eq-chain temp (`const tmp,k`) lives at `switch_scratch` = registers_used.
                // When that's ≥16 (or the key register is high), route through the 2 reserved low
                // scratch (sb = temp, sb+1 = spilled key): the nibble `if-eq` then fits. For
                // ≤16-register methods this path isn't taken — byte-identical to before.
                let (tmp, key_use) = match spill_base {
                    Some(sb) if switch_scratch >= 16 || key_hi => {
                        let ku = if key_hi {
                            emit_copy(&mut insns, sb + 1, key, false, false, na, est)?;
                            sb + 1
                        } else {
                            key
                        };
                        (sb, ku)
                    }
                    _ => {
                        if switch_scratch >= 16 {
                            bail!("ssa dexbuilder: switch scratch register {switch_scratch} >= 16 (if-eq is nibble-encoded)");
                        }
                        used_switch_scratch = true;
                        (switch_scratch, key)
                    }
                };
                for &(k, target) in cases {
                    emit_const_int(&mut insns, tmp, k, na, est);
                    insns.push(0x32 | (nib(key_use)? << 8) | (nib(tmp)? << 12)); // if-eq key, tmp
                    let off = insns.len();
                    insns.push(0);
                    fixups.push((off, target, false));
                }
                let goff = insns.len();
                insns.push(0x28); // goto default
                fixups.push((goff, *default, true));
            }
        }
    }

    // Emit critical taken-edge trampolines (after all blocks): `moves; goto target`. Each
    // got synthetic id f.blocks.len()+i; record its offset in block_unit (extended) so the
    // redirected taken branch resolves to it.
    for (target, moves) in &trampolines {
        block_unit.push(insns.len());
        emit_move_list(f, &mut insns, alloc, moves, &mut phi_scratch_words, frame_hint)?;
        let off = insns.len();
        insns.push(0x28); // goto target
        fixups.push((off, *target, true));
    }

    // Branch relaxation (goto 8-bit → goto/16): a `goto` (0x28, 1 word, ±127) whose
    // target is too far is widened to `goto/16` (0x29, 2 words: opcode word + signed-16
    // offset word). Widening inserts a word, shifting every later word-offset, which can
    // push other gotos over ±127 — so iterate to a fixpoint. The wide set only grows, so
    // it converges. (16-bit reaches ±32k; a goto past that would need goto/32 — bail.)
    loop {
        let mut widened = false;
        for fi in 0..fixups.len() {
            let (off, target, is_goto) = fixups[fi];
            if !is_goto || (insns[off] & 0xff) == 0x29 {
                continue; // not a goto, or already widened
            }
            let rel = block_unit[target] as i32 - off as i32;
            if (-128..=127).contains(&rel) {
                continue; // fits the 8-bit form
            }
            if !(-32768..=32767).contains(&rel) {
                bail!("ssa dexbuilder: goto offset {rel} needs goto/32 (not yet supported)");
            }
            // Widen: opcode word → 0x29 (offset goes in the inserted next word).
            insns[off] = 0x29;
            insns.insert(off + 1, 0);
            // Bump every word-offset reference strictly after the inserted word.
            for bu in block_unit.iter_mut() {
                if *bu > off {
                    *bu += 1;
                }
            }
            for (o, _, _) in fixups.iter_mut() {
                if *o > off {
                    *o += 1;
                }
            }
            for pf in pool_fixups.iter_mut() {
                if pf.unit > off {
                    pf.unit += 1;
                }
            }
            for (p, _) in positions.iter_mut() {
                if *p as usize > off {
                    *p += 1;
                }
            }
            for (_, ds, de) in throw_spans.iter_mut() {
                if *ds > off {
                    *ds += 1;
                }
                if *de > off {
                    *de += 1;
                }
            }
            widened = true;
            break; // offsets changed — restart the scan
        }
        if !widened {
            break;
        }
    }

    // Resolve branch offsets.
    for (off, target, is_goto) in fixups {
        let tgt = block_unit[target] as i32;
        if is_goto {
            let rel = tgt - off as i32; // goto offset is from the op word itself
            if (insns[off] & 0xff) == 0x29 {
                insns[off + 1] = rel as i16 as u16; // goto/16: signed-16 offset in word 2
            } else {
                if !(-128..=127).contains(&rel) {
                    bail!("ssa dexbuilder: goto offset {rel} unexpectedly un-relaxed");
                }
                insns[off] = 0x28 | (((rel as i8) as u8 as u16) << 8);
            }
        } else {
            let rel = tgt - (off as i32 - 1); // if offset is from the op word (off-1)
            if !(-32768..=32767).contains(&rel) {
                bail!("ssa dexbuilder: if/branch offset {rel} needs 32-bit form (not yet supported)");
            }
            insns[off] = rel as i16 as u16;
        }
    }

    // try_items: each region narrows to the DEX span of its guarded throwing
    // instructions ([first_start, last_end)); the handler points at the catch block.
    let mut tries: Vec<TryItem> = Vec::new();
    for r in &f.exc_regions {
        let mut lo = usize::MAX;
        let mut hi = 0usize;
        for &(jpc, ds, de) in &throw_spans {
            if (r.start_pc..r.end_pc).contains(&(jpc as usize)) {
                lo = lo.min(ds);
                hi = hi.max(de);
            }
        }
        if lo == usize::MAX {
            bail!("ssa: try region has no guarded throwing instruction");
        }
        // A catch-all (catch_type None — finally / synchronized) goes in catch_all_addr
        // with no typed handler; a typed catch goes in the handlers list.
        let handler_addr = block_unit[r.handler_block] as u32;
        let (handlers, catch_all_addr) = match &r.catch_type {
            Some(ct) => (vec![CatchHandler { exception_type: ct.clone(), addr: handler_addr }], None),
            None => (vec![], Some(handler_addr)),
        };
        tries.push(TryItem {
            start_addr: lo as u32,
            insn_count: (hi - lo) as u16,
            handlers,
            catch_all_addr,
        });
    }
    tries.sort_by_key(|t| t.start_addr);

    // Reserve scratch above the allocated set: +1 for a switch's if-eq-chain temp, and
    // the range-invoke marshalling block (both based at `registers_used`, temporally
    // disjoint, so the frame just needs to cover the larger).
    let scratch = u16::from(used_switch_scratch)
        .max(range_block_words)
        .max(phi_scratch_words);
    let registers_size = (alloc.registers_used + scratch).max(f.num_arg_registers);
    // Scratch-aware spill/range retry. Scratch (range-invoke / switch / φ) inflates
    // `registers_size` beyond `alloc.registers_used`, pushing ARGUMENTS to the top of the larger
    // frame. The pre-emit decisions (the iget/iput spill in `dex_method_ssa`, and `emit_invoke`'s
    // 35c-vs-range choice) both use `registers_used` and so miss an argument whose REAL register
    // only reaches ≥16 once scratch is added — e.g. `this.field`, or an invoke arg, in a method
    // that ALSO makes a wide / many-arg range invoke (whose block is the scratch doing the
    // pushing). Now that the true `registers_size` is known, re-check and re-emit with it as the
    // frame hint. This ONLY fires for methods that would otherwise bail at `remap_insns` below —
    // it never turns a passing method into a bailing one. Two remedies:
    //   • a high FIELD operand needs 2 reserved LOW scratch (22c has no range form) — reserve and
    //     re-emit with `spill_base`; the reserve bumps `registers_used` by 2 (scratch unchanged),
    //     so the re-emit frame is exactly `registers_size + 2`. `num_arg <= 14` keeps the 2 slots
    //     remap-clean. (A high LOCAL field operand would have bailed earlier in `emit_field`'s
    //     `nib()`, so this targets argument operands.)
    //   • a high INVOKE arg needs only the true frame: `emit_invoke` already marshals args into a
    //     consecutive range block and emits `invoke/range`, but only when it SEES an arg as high —
    //     so just re-emit with the frame hint (no reserve; the gather reuses the existing block).
    //   • a two-register if-test (22t, no wider form) with a high operand ALSO needs the 2 low
    //     scratch — the If terminator moves the high operand(s) down before the compare.
    if frame_hint.is_none() && registers_size > 16 {
        let na = f.num_arg_registers;
        let high = |r: u16| r != NO_REG && crate::regalloc::remap_register(r, na, registers_size) >= 16;
        let field_op_high = f.values.iter().enumerate().any(|(i, val)| match &val.op {
            SsaOp::GetField { dex_op, obj, .. } if *dex_op != 0x53 => {
                high(alloc.reg[i]) || high(alloc.reg[*obj as usize])
            }
            SsaOp::PutField { dex_op, obj, value, .. }
                if *dex_op != 0x5a && !f.values[*value as usize].wide =>
            {
                high(alloc.reg[*value as usize]) || high(alloc.reg[*obj as usize])
            }
            // array-length (0x21) / instance-of (0x20): same nibble-form spill (dest + ref operand)
            SsaOp::ArrayLength { array, .. } => high(alloc.reg[i]) || high(alloc.reg[*array as usize]),
            SsaOp::InstanceOf { obj, .. } => high(alloc.reg[i]) || high(alloc.reg[*obj as usize]),
            // iget-wide (0x53) / iput-wide (0x5a): only the obj-high / wide-dest-low case is
            // spillable (the wide pair stays low; a high wide pair would need a 2-reg scratch pair).
            SsaOp::GetField { dex_op, obj, .. } if *dex_op == 0x53 => {
                high(alloc.reg[*obj as usize]) && !high(alloc.reg[i]) && !high(alloc.reg[i] + 1)
            }
            SsaOp::PutField { dex_op, obj, value, .. } if *dex_op == 0x5a => {
                let vr = alloc.reg[*value as usize];
                high(alloc.reg[*obj as usize]) && !high(vr) && !high(vr + 1)
            }
            _ => false,
        });
        let if_test_high = f.blocks.iter().any(|blk| match &blk.term {
            Terminator::If { jvm_op, operands, .. } => {
                matches!(crate::bootstrap::cond_branch_dex_op(*jvm_op), Some((_, true)))
                    && operands.iter().any(|&o| high(alloc.reg[o as usize]))
            }
            _ => false,
        });
        // A switch lowers to a `const tmp,k; if-eq key,tmp` chain whose temp sits at
        // `registers_used`; when that's ≥16 (or the key register is high) it needs the 2 low scratch.
        let switch_high = f.blocks.iter().any(|blk| match &blk.term {
            Terminator::Switch { value, .. } => {
                alloc.registers_used >= 16 || high(alloc.reg[*value as usize])
            }
            _ => false,
        });
        // Invoke args (→ range form), φ-move operands and check-cast objects (→ …/from16 copy)
        // self-widen given the true frame — no reserve needed, just re-emit with the hint.
        let move_or_invoke_high = f.values.iter().enumerate().any(|(i, val)| match &val.op {
            SsaOp::Invoke { args, .. } => args.iter().any(|&a| high(alloc.reg[a as usize])),
            SsaOp::Phi { operands, .. } => {
                high(alloc.reg[i]) || operands.iter().any(|&o| high(alloc.reg[o as usize]))
            }
            SsaOp::CheckCast { obj, .. } => high(alloc.reg[i]) || high(alloc.reg[*obj as usize]),
            _ => false,
        });
        if spill_base.is_none() && na <= 14 && (field_op_high || if_test_high || switch_high) {
            let mut alloc2 = alloc.clone();
            reserve_scratch(&mut alloc2, na, 2);
            // frame_hint covers the +2; emit_invoke / emit_copy / if-test / switch spill use it too.
            return build_dex(f, num, &alloc2, line_numbers, params, Some(na), Some(registers_size + 2));
        } else if move_or_invoke_high {
            return build_dex(f, num, alloc, line_numbers, params, spill_base, Some(registers_size));
        }
    }
    // Safety net: every register operand must be in allocated space [0, registers_size).
    // An out-of-range operand means a value got NO_REG (e.g. a const wrongly rematerialized
    // despite a register-requiring use) and emitted garbage — bail rather than miscompile.
    if let Some(max_reg) = crate::regalloc::max_register_used(&insns) {
        if max_reg >= registers_size {
            bail!("ssa: register operand v{max_reg} out of range (>= {registers_size}) — a value got no register (NO_REG)");
        }
    }
    crate::regalloc::remap_insns(&mut insns, f.num_arg_registers, registers_size)?;
    let debug_info = crate::bootstrap::build_debug_info(&positions, params);
    Ok(CodeItem {
        registers_size,
        ins_size: f.num_arg_registers,
        outs_size: outs,
        insns,
        fixups: pool_fixups,
        tries,
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
    range_block_words: &mut u16,
    frame_hint: Option<u16>,
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
    // The compact 35c form encodes each arg in a 4-bit nibble: usable only when ≤5 args AND every
    // arg fits a nibble — both its ALLOCATED number (else the nibble emit truncates it before remap
    // reads it) AND its FINAL args-high number (else remap overflows). Args remap UP, so an arg with
    // a low allocated number can still land ≥16 after remap; checking only the allocated number (as
    // before) wrongly kept 35c and then bailed in remap. Estimate the final with the scratch-free
    // size (it only UNDER-counts args, so a borderline case may still bail in remap — safe, never a
    // miscompile). For ≤16-register methods every final is ≤15, so this never newly forces range —
    // those methods stay byte-identical; only >16-register methods are affected.
    let num_arg = f.num_arg_registers;
    // Use the true frame (registers_size) when re-emitting after a scratch-aware retry so an arg
    // pushed ≥16 by another op's scratch is seen as high and forces the range form; else the
    // scratch-free estimate (only ever UNDER-counts, so a missed case bails safely in remap).
    let regs_size_est = frame_hint.unwrap_or(alloc.registers_used.max(num_arg));
    let final_of = |r: u16| -> u16 {
        if num_arg == 0 || regs_size_est == num_arg {
            r
        } else {
            crate::regalloc::remap_register(r, num_arg, regs_size_est)
        }
    };
    if regs.len() > 5 || regs.iter().any(|&r| r > 15 || final_of(r) > 15) {
        // RANGE FORM: marshal the args into a CONSECUTIVE scratch block just above the
        // allocated set, then `invoke-*/range` over it. The block stays consecutive under
        // the args-high remap (every non-arg register shifts by the same -num_arg), so
        // remapping the move dests + the invoke's CCCC start register is sound. The block
        // is above registers_used, so it clobbers no live value; registers_size is bumped
        // to cover it (range_block_words). invoke/range's CCCC is 16-bit, but the move
        // dest (AA, 22x) is 8-bit, so bail if the block top exceeds 255 (that genuinely
        // needs spilling).
        let base = alloc.registers_used;
        let nwords = regs.len() as u16;
        if base as usize + regs.len() > 256 {
            bail!("ssa dexbuilder: range-invoke block top > 255 (needs spilling)");
        }
        let mut off: u16 = 0;
        for &a in args {
            let src = reg(a);
            let dest = base + off;
            let (mv, w): (u16, u16) = if f.values[a as usize].wide {
                (0x05, 2) // move-wide/from16
            } else if f.values[a as usize].is_ref {
                (0x08, 1) // move-object/from16
            } else {
                (0x02, 1) // move/from16
            };
            insns.push(mv | (dest << 8)); // 22x: AA dest in word0 high byte
            insns.push(src); // BBBB src in word1
            off += w;
        }
        *range_block_words = (*range_block_words).max(nwords);
        if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
            positions.push((insns.len() as u32, line));
        }
        let range_op = 0x74 + (dex_op - 0x6e); // 0x6e→0x74 … 0x72→0x78 (virtual/super/direct/static/interface)
        insns.push(range_op | (nwords << 8)); // AA = arg-register count
        let method_unit = insns.len();
        insns.push(0); // method-ref placeholder (word1), patched via the fixup
        insns.push(base); // CCCC = first register of the consecutive arg block (word2)
        pool_fixups.push(Fixup { unit: method_unit, item: ItemRef::Method(method.clone()), wide: false });
        *outs = (*outs).max(nwords);
        if let Some(rk) = ret {
            let dest = reg(v);
            let mvr: u16 = if rk.wide { 0x0b } else if rk.is_ref { 0x0c } else { 0x0a };
            insns.push(mvr | (dest << 8));
        }
        return Ok(());
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
    spill_base: Option<u16>,
    frame_hint: Option<u16>,
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
    // >16-register iget/iput spill: move a high operand through reserved scratch (sb, sb+1).
    if let Some(sb) = spill_base {
        let na = f.num_arg_registers;
        // Use the true frame size when re-emitting after a scratch-aware retry so an argument
        // pushed ≥16 by scratch inflation is spilled; else fall back to `registers_used`.
        let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
        let high = |r: u16| r >= 16 || crate::regalloc::remap_register(r, na, est) >= 16;
        match &f.values[v as usize].op {
            SsaOp::GetField { obj, .. } if dex_op != 0x53 && (high(reg(v)) || high(reg(*obj))) => {
                // The object operand is ALWAYS a reference, so it must move via
                // move-object/from16 (0x08) — never move/from16 (0x02, primitives only), which
                // ART's verifier rejects ("copy1 … type=Reference"). The field VALUE (dest of
                // iget) is a reference only for iget-object (0x54); else it's a 32-bit primitive.
                let or = reg(*obj);
                let or_use = if high(or) {
                    insns.push(0x08 | ((sb + 1) << 8));
                    insns.push(or);
                    sb + 1
                } else {
                    or
                };
                let dr = reg(v);
                let dr_use = if high(dr) { sb } else { dr };
                insns.push(dex_op | (nib(dr_use)? << 8) | (nib(or_use)? << 12));
                let unit = insns.len();
                insns.push(0);
                pool_fixups.push(Fixup { unit, item: ItemRef::Field(field.clone()), wide: false });
                if high(dr) {
                    let mv = if dex_op == 0x54 { 0x08 } else { 0x02 };
                    insns.push(mv | (dr << 8));
                    insns.push(dr_use);
                }
                return Ok(());
            }
            SsaOp::PutField { obj, value, .. }
                if dex_op != 0x5a
                    && !f.values[*value as usize].wide
                    && (high(reg(*value)) || high(reg(*obj))) =>
            {
                // value is a reference only for iput-object (0x5c); object is always a reference.
                let vr = reg(*value);
                let vr_use = if high(vr) {
                    let mv = if dex_op == 0x5c { 0x08 } else { 0x02 };
                    insns.push(mv | (sb << 8));
                    insns.push(vr);
                    sb
                } else {
                    vr
                };
                let or = reg(*obj);
                let or_use = if high(or) {
                    insns.push(0x08 | ((sb + 1) << 8));
                    insns.push(or);
                    sb + 1
                } else {
                    or
                };
                insns.push(dex_op | (nib(vr_use)? << 8) | (nib(or_use)? << 12));
                let unit = insns.len();
                insns.push(0);
                pool_fixups.push(Fixup { unit, item: ItemRef::Field(field.clone()), wide: false });
                return Ok(());
            }
            // iget-wide (0x53): the dest is a WIDE pair. Only the obj-high / dest-low case spills
            // here — move the object reference down and read the wide value into its (low) pair. A
            // high wide dest needs a 2-register scratch PAIR (not yet handled): the guard excludes
            // it so it falls through to the normal emit and bails via `nib()` (never miscompiles).
            SsaOp::GetField { obj, .. }
                if dex_op == 0x53 && high(reg(*obj)) && !high(reg(v)) && !high(reg(v) + 1) =>
            {
                insns.push(0x08 | ((sb + 1) << 8)); // move-object/from16 obj → sb+1
                insns.push(reg(*obj));
                insns.push(0x53 | (nib(reg(v))? << 8) | (nib(sb + 1)? << 12));
                let unit = insns.len();
                insns.push(0);
                pool_fixups.push(Fixup { unit, item: ItemRef::Field(field.clone()), wide: false });
                return Ok(());
            }
            // iput-wide (0x5a): the value is a WIDE pair; same obj-high / value-low spill.
            SsaOp::PutField { obj, value, .. }
                if dex_op == 0x5a
                    && high(reg(*obj))
                    && !high(reg(*value))
                    && !high(reg(*value) + 1) =>
            {
                insns.push(0x08 | ((sb + 1) << 8)); // move-object/from16 obj → sb+1
                insns.push(reg(*obj));
                insns.push(0x5a | (nib(reg(*value))? << 8) | (nib(sb + 1)? << 12));
                let unit = insns.len();
                insns.push(0);
                pool_fixups.push(Fixup { unit, item: ItemRef::Field(field.clone()), wide: false });
                return Ok(());
            }
            _ => {}
        }
    }
    match &f.values[v as usize].op {
        // 21c: sget/sput AA = dest/value.
        SsaOp::GetStatic { .. } => insns.push(dex_op | (reg(v) << 8)),
        SsaOp::PutStatic { value, .. } => insns.push(dex_op | (reg(*value) << 8)),
        // 22c: iget/iput A = dest/value (low nibble), B = object (high nibble). No wider
        // form exists, so a register ≥16 must spill — bail loudly (never truncate).
        SsaOp::GetField { obj, .. } => {
            insns.push(dex_op | (nib(reg(v))? << 8) | (nib(reg(*obj))? << 12));
        }
        SsaOp::PutField { obj, value, .. } => {
            insns.push(dex_op | (nib(reg(*value))? << 8) | (nib(reg(*obj))? << 12));
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
    spill_base: Option<u16>,
    frame_hint: Option<u16>,
) -> Result<()> {
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
        // array-length (0x21, 12x): A = dest (low nibble), B = array (high nibble). No wider form,
        // so a high dest/array routes through the 2 low scratch reserved by the build_dex retry.
        SsaOp::ArrayLength { array, .. } => {
            let (dest, arr) = (reg(v), reg(*array));
            if let Some(sb) = spill_base {
                let na = f.num_arg_registers;
                let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
                let (dest_use, arr_use, reload) = spill_dest_obj(insns, sb, na, est, dest, arr)?;
                insns.push(0x21 | (nib(dest_use)? << 8) | (nib(arr_use)? << 12));
                if reload {
                    emit_copy(insns, dest, dest_use, false, false, na, est)?;
                }
            } else {
                insns.push(0x21 | (nib(dest)? << 8) | (nib(arr)? << 12));
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Emits object allocation: `new-instance dest, type@` (21c) or `new-array dest,
/// size, type@` (22c). Both are throwing → a debug position; the type-ref word is
/// a fixup placeholder.
fn emit_alloc(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let (type_desc, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::NewInstance { type_desc, jvm_pc } | SsaOp::NewArray { type_desc, jvm_pc, .. } => {
            (type_desc.clone(), *jvm_pc)
        }
        _ => unreachable!(),
    };
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    match &f.values[v as usize].op {
        SsaOp::NewInstance { .. } => insns.push(0x22 | (reg(v) << 8)), // 21c (AA, 8-bit)
        // 22c: new-array dest (low nibble) + size (high nibble). No wider form, so a register
        // ≥16 must spill — bail loudly (never truncate).
        SsaOp::NewArray { size, .. } => {
            insns.push(0x23 | (nib(reg(v))? << 8) | (nib(reg(*size))? << 12));
        }
        _ => unreachable!(),
    }
    let unit = insns.len();
    insns.push(0); // type-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::Type(type_desc), wide: false });
    Ok(())
}

/// Emits `check-cast vAA, type@` (21c). check-cast is IN-PLACE in DEX (it asserts the
/// type of one register, leaving its value unchanged), but the SSA result is a fresh
/// value that may have been allocated a different register than the object. So if the
/// result register differs, copy the object into it first (`move-object`), then assert
/// on the result register. Throwing (ClassCastException) → records a debug position.
fn emit_check_cast(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
    frame_hint: Option<u16>,
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let (obj, type_desc, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::CheckCast { obj, type_desc, jvm_pc } => (*obj, type_desc.clone(), *jvm_pc),
        _ => unreachable!(),
    };
    let (dest, src) = (reg(v), reg(obj));
    if dest != src {
        // Copy the object into the result register before the in-place cast. emit_copy picks the
        // 12x move-object or the wider move-object/from16 (both remapped by reg_fields) per the
        // registers' final args-high numbers.
        let num_arg = f.num_arg_registers;
        let est = frame_hint.unwrap_or(alloc.registers_used.max(num_arg));
        emit_copy(insns, dest, src, false, true, num_arg, est)?;
    }
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    insns.push(0x1f | (dest << 8));
    let unit = insns.len();
    insns.push(0); // type-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::Type(type_desc), wide: false });
    Ok(())
}

/// Emits `instance-of vA, vB, type@` (22c, nibble dest + nibble src). Non-throwing
/// (null → false), so no debug position. Result is an int (boolean 0/1).
fn emit_instance_of(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    _positions: &mut [(u32, u32)],
    _line_numbers: &[(u16, u16)],
    spill_base: Option<u16>,
    frame_hint: Option<u16>,
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let (obj, type_desc) = match &f.values[v as usize].op {
        SsaOp::InstanceOf { obj, type_desc, .. } => (*obj, type_desc.clone()),
        _ => unreachable!(),
    };
    let (dest, src) = (reg(v), reg(obj));
    // instance-of (22c) has no wider form, so a high dest/obj routes through the 2 low scratch.
    let (dest_use, src_use, reload) = if let Some(sb) = spill_base {
        let na = f.num_arg_registers;
        let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
        spill_dest_obj(insns, sb, na, est, dest, src)?
    } else {
        (dest, src, false)
    };
    insns.push(0x20 | (nib(dest_use)? << 8) | (nib(src_use)? << 12));
    let unit = insns.len();
    insns.push(0); // type-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::Type(type_desc), wide: false });
    if reload {
        let na = f.num_arg_registers;
        let est = frame_hint.unwrap_or(alloc.registers_used.max(na));
        emit_copy(insns, dest, dest_use, false, false, na, est)?; // move/from16 dest <- scratch
    }
    Ok(())
}

/// Emits `const-string dest, string@` (21c). Throwing in d8's model, so it records
/// a debug position; the string-ref word is a fixup placeholder.
fn emit_const_string(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) {
    let (value, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::ConstString { value, jvm_pc } => (value.clone(), *jvm_pc),
        _ => unreachable!(),
    };
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    insns.push(0x1a | (alloc.reg[v as usize] << 8));
    let unit = insns.len();
    insns.push(0); // string-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::String(value), wide: false });
}

/// Emits `const-class dest, type@` (0x1c, 21c) — the `X.class` literal. Throwing
/// (class init), so it records a debug position; the type-ref word is a fixup.
fn emit_const_class(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    v: ValId,
    pool_fixups: &mut Vec<Fixup>,
    positions: &mut Vec<(u32, u32)>,
    line_numbers: &[(u16, u16)],
) {
    let (type_desc, jvm_pc) = match &f.values[v as usize].op {
        SsaOp::ConstClass { type_desc, jvm_pc } => (type_desc.clone(), *jvm_pc),
        _ => unreachable!(),
    };
    if let Some(line) = crate::bootstrap::line_for(line_numbers, jvm_pc) {
        positions.push((insns.len() as u32, line));
    }
    insns.push(0x1c | (alloc.reg[v as usize] << 8));
    let unit = insns.len();
    insns.push(0); // type-ref placeholder, patched via the fixup
    pool_fixups.push(Fixup { unit, item: ItemRef::Type(type_desc), wide: false });
}

/// Inserts φ-resolution moves at the end of block `b` (a predecessor): for each
/// successor's φ whose operand from `b` is in a different register than the φ, emit
/// `move/move-wide/move-object φ-reg ← operand-reg` (12x). Bails for the cases not
/// yet supported: a move needed on a BRANCHING edge (needs edge-splitting) or a
/// register > 15 (12x is nibble-encoded) or a parallel-copy cycle.
/// The (φ, operand) move pairs needed on the edge `pred → s` (φ operand allocated to a
/// different register than the φ).
fn phi_moves_for_edge(f: &SsaFn, alloc: &Allocation, pred: usize, s: usize) -> Vec<(ValId, ValId)> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let pred_idx = match f.blocks[s].preds.iter().position(|&p| p == pred) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let mut moves = Vec::new();
    for &phi in &f.blocks[s].phis {
        if let SsaOp::Phi { operands, .. } = &f.values[phi as usize].op {
            if let Some(&o) = operands.get(pred_idx) {
                if reg(o) != reg(phi) {
                    moves.push((phi, o));
                }
            }
        }
    }
    moves
}

/// Emits one copy `dst <- src` (`move`/`move-wide`/`move-object`), picking the compact 12x
/// nibble form when every occupied register fits a nibble — both ALLOCATED (else the 12x emit
/// truncates before remap) AND FINAL after the args-high remap — and otherwise the wider
/// `…/from16` form (move 0x02 / move-wide 0x05 / move-object 0x08, 22x: 8-bit AA dst + 16-bit
/// BBBB src). Registers are written in ALLOCATED space; `remap_insns` remaps both forms in place
/// (its `reg_fields` covers 0x01/0x04/0x07 12x AND 0x02/0x05/0x08 22x). `est` is the frame size
/// used for the FINAL test (the true `registers_size` on a scratch-aware retry, else
/// `registers_used`). For ≤16-register methods every final is ≤15 so this always picks 12x —
/// byte-identical to before; only >16-register methods take the wide form. Bails only if even the
/// wide form can't hold it (allocated dst ≥256, the 8-bit AA field — genuinely needs move/16).
fn emit_copy(
    insns: &mut Vec<u16>,
    dst: u16,
    src: u16,
    wide: bool,
    isref: bool,
    num_arg: u16,
    est: u16,
) -> Result<()> {
    let remap = |r: u16| crate::regalloc::remap_register(r, num_arg, est);
    // The value occupies `r` (and `r+1` if wide); a nibble form needs every occupied register
    // ≤15 in BOTH allocated and final space.
    let hi = |r: u16| {
        let top = if wide { r + 1 } else { r };
        r > 15 || top > 15 || remap(r) > 15 || remap(top) > 15
    };
    if !hi(dst) && !hi(src) {
        let op: u16 = if wide { 0x04 } else if isref { 0x07 } else { 0x01 };
        insns.push(op | ((dst & 0xf) << 8) | ((src & 0xf) << 12));
    } else {
        if dst > 0xff {
            bail!("ssa dexbuilder: copy dst v{dst} ≥256 (8-bit /from16 AA) — needs move/16");
        }
        let op: u16 = if wide { 0x05 } else if isref { 0x08 } else { 0x02 };
        insns.push(op | (dst << 8)); // 22x: AA dst in word0 high byte (allocated; remapped later)
        insns.push(src); // BBBB src in word1 (allocated; remapped later)
    }
    Ok(())
}

/// Routes a nibble-form `op dest, obj[, ref]` (the iget / instance-of / array-length shape: a
/// 32-bit result `dest` plus one object/array operand `obj`) through the 2 low scratch registers
/// reserved by the build_dex retry (`sb`, `sb+1`) when an operand's FINAL args-high register is
/// ≥16. Spills the object operand DOWN to `sb+1` (move-object/from16 — `obj` is always a
/// reference) and, if `dest` is high, redirects the op's output to `sb` and signals a reload.
/// Returns `(dest_use, obj_use, reload_dest)`: the caller emits the op with `dest_use`/`obj_use`,
/// then if `reload_dest` copies `sb` back into `dest` (move/from16 — `dest` is the 32-bit result).
fn spill_dest_obj(
    insns: &mut Vec<u16>,
    sb: u16,
    na: u16,
    est: u16,
    dest: u16,
    obj: u16,
) -> Result<(u16, u16, bool)> {
    let hi = |r: u16| r >= 16 || crate::regalloc::remap_register(r, na, est) >= 16;
    let obj_use = if hi(obj) {
        emit_copy(insns, sb + 1, obj, false, true, na, est)?;
        sb + 1
    } else {
        obj
    };
    let (dest_use, reload) = if hi(dest) { (sb, true) } else { (dest, false) };
    Ok((dest_use, obj_use, reload))
}

/// Emits a parallel-copy of φ-moves (`move`/`move-wide`/`move-object`). Each copy uses the 12x
/// nibble form or the wider `…/from16` form per `emit_copy`. Bails on a self-overlapping wide
/// move or a cycle whose scratch temp can't be encoded.
fn emit_move_list(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    moves: &[(ValId, ValId)],
    scratch_words: &mut u16,
    frame_hint: Option<u16>,
) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let num_arg = f.num_arg_registers;
    let est = frame_hint.unwrap_or(alloc.registers_used.max(num_arg));
    // A φ-move set is a PARALLEL copy: every source is read in the original register state,
    // all destinations written "simultaneously". Sequentializing requires (1) ordering so a
    // move's source isn't clobbered by an earlier move (chains: a←b, b←c emits a←b first),
    // and (2) a scratch temp to break cycles (a swap r0↔r1 needs t←r0; r0←r1; r1←t). The
    // temp is `registers_used` — the allocator's high-water mark, guaranteed dead (above
    // every allocated value); registers_size is bumped to cover it via *scratch_words.
    #[derive(Clone, Copy)]
    struct M {
        dst: u16,
        src: u16,
        wide: bool,
        isref: bool,
    }
    // Register range a value occupies (wide = 2 consecutive registers).
    let occ = |base: u16, wide: bool| (base, if wide { base + 1 } else { base });
    let overlap = |a: (u16, u16), b: (u16, u16)| a.0 <= b.1 && b.0 <= a.1;
    let mut pend: Vec<M> = Vec::new();
    for &(phi, o) in moves {
        let (dst, src) = (reg(phi), reg(o));
        let wide = f.values[phi as usize].wide;
        let isref = f.values[phi as usize].is_ref;
        if dst == src {
            continue; // self-move: no-op
        }
        // A register ≥16 (allocated or remapped) is fine — emit_copy widens to …/from16 below.
        // A move whose dst range overlaps its OWN src range (e.g. wide r2←r3) would corrupt
        // mid-copy — pathological for a well-formed φ; refuse rather than miscompile.
        if overlap(occ(dst, wide), occ(src, wide)) {
            bail!("ssa dexbuilder: φ-move self-overlapping wide registers (not yet supported)");
        }
        pend.push(M { dst, src, wide, isref });
    }
    let mut out: Vec<M> = Vec::new();
    // φ dests are distinct, so `n.dst != m.dst` uniquely identifies "a different move".
    let guard = pend.len() * 4 + 4; // sequentialization terminates; this is a safety net.
    let mut iters = 0usize;
    while !pend.is_empty() {
        iters += 1;
        if iters > guard {
            bail!("ssa dexbuilder: φ-move sequentialization did not converge");
        }
        // A move is READY when its destination isn't (part of) any OTHER pending move's
        // source — overwriting it then clobbers no value still needed. Drains chains.
        let ready = pend.iter().position(|m| {
            let dr = occ(m.dst, m.wide);
            !pend
                .iter()
                .any(|n| n.dst != m.dst && overlap(dr, occ(n.src, n.wide)))
        });
        if let Some(i) = ready {
            out.push(pend.remove(i));
            continue;
        }
        // No ready move ⇒ the remainder is one or more cycles. Break one: copy pend[0]'s
        // source into the scratch temp, then redirect every reader of that source to the
        // temp — which frees the register feeding pend[0], making progress next round. A
        // wide value needs a 2-register temp (both above the high-water mark → dead).
        let m0 = pend[0];
        let w: u16 = if m0.wide { 2 } else { 1 };
        let temp = alloc.registers_used;
        // The cycle-break temp copies go through emit_copy too, so the temp may be ≥16 (…/from16);
        // bail only if its allocated number won't fit the 8-bit /from16 dst (needs move/16).
        if temp + (w - 1) > 0xff {
            bail!("ssa dexbuilder: φ-move cycle scratch register {temp} ≥256 (needs move/16)");
        }
        *scratch_words = (*scratch_words).max(w);
        out.push(M { dst: temp, src: m0.src, wide: m0.wide, isref: m0.isref });
        let sr = occ(m0.src, m0.wide);
        for n in pend.iter_mut() {
            if overlap(sr, occ(n.src, n.wide)) {
                if n.src != m0.src || n.wide != m0.wide {
                    bail!("ssa dexbuilder: φ-move cycle partial-overlap (not yet supported)");
                }
                n.src = temp;
            }
        }
    }
    for m in &out {
        emit_copy(insns, m.dst, m.src, m.wide, m.isref, num_arg, est)?;
    }
    Ok(())
}

/// φ-moves on this (predecessor) block's OUTGOING edges, emitted at its end. A move on a
/// branch edge P→S (P has >1 successor) can't go at P's end; if S has a SINGLE predecessor
/// it's emitted at S's entry instead (emit_entry_phi_moves), so only a TRUE critical edge
/// (P>1 succ AND S>1 pred) still bails.
fn emit_phi_moves(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    b: usize,
    scratch_words: &mut u16,
    frame_hint: Option<u16>,
) -> Result<()> {
    // A BRANCHING block's edge φ-moves can't sit at its end (they'd run on every arm).
    // Each is handled elsewhere: a single-pred successor at its own entry
    // (emit_entry_phi_moves); a critical (multi-pred) successor in the If terminator
    // (fallthrough inline, or taken via a split block). So nothing to do here.
    if f.blocks[b].succ.len() > 1 {
        return Ok(());
    }
    // Single-successor block: the move runs unconditionally at the block's end.
    for &s in &f.blocks[b].succ.clone() {
        let moves = phi_moves_for_edge(f, alloc, b, s);
        if !moves.is_empty() {
            emit_move_list(f, insns, alloc, &moves, scratch_words, frame_hint)?;
        }
    }
    Ok(())
}

/// φ-moves emitted at THIS block's entry (before its body): the case where this block has
/// a single predecessor that BRANCHES (so the moves can't sit at the pred's end, but the
/// edge isn't critical because this block is entered only from that pred). Skips handler
/// blocks (entered via exception, not a normal pred edge).
fn emit_entry_phi_moves(
    f: &SsaFn,
    insns: &mut Vec<u16>,
    alloc: &Allocation,
    s: usize,
    scratch_words: &mut u16,
    frame_hint: Option<u16>,
) -> Result<()> {
    if f.blocks[s].preds.len() != 1 || f.caught[s].is_some() {
        return Ok(());
    }
    let p = f.blocks[s].preds[0];
    if f.blocks[p].succ.len() <= 1 {
        return Ok(()); // pred single-succ → handled at the pred's end
    }
    let moves = phi_moves_for_edge(f, alloc, p, s);
    emit_move_list(f, insns, alloc, &moves, scratch_words, frame_hint)
}

/// A 4-bit register nibble. The 12x/22c/22t/22s/11n forms encode a register in a 4-bit
/// nibble that can't hold a value ≥16. Emit writes ALLOCATED registers and `remap_insns`
/// remaps them in place afterward — but a nibble masked at emit time loses the high bits
/// BEFORE remap can see them, so the remap's fail-not-truncate guard can't catch it. This
/// helper bails LOUDLY instead, for nibble forms that have NO wider alternative (iget/iput,
/// if-test, array-length, new-array, unops, lit16). Forms that CAN widen (binop→3addr,
/// const/4→const/16, invoke→range) gate on `r <= 15` and pick the wide form directly.
/// For ≤16-register methods every allocated register is <16, so `nib` is the identity and
/// the emitted code is byte-identical to before — only >16-register methods can hit a bail.
fn nib(r: u16) -> Result<u16> {
    if r > 15 {
        bail!("ssa dexbuilder: register v{r} does not fit a 4-bit nibble form (no wider form exists) — needs spilling");
    }
    Ok(r)
}

/// Emits the instruction defining `v` (the result lands in `reg(v)`).
fn emit_value(f: &SsaFn, insns: &mut Vec<u16>, alloc: &Allocation, v: ValId, frame_hint: Option<u16>) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    let dest = reg(v);
    let num_arg = f.num_arg_registers;
    let est = frame_hint.unwrap_or(alloc.registers_used.max(num_arg));
    match &f.values[v as usize].op {
        SsaOp::ConstInt(c) => emit_const_int(insns, dest, *c, num_arg, est),
        SsaOp::ConstLong(c) => emit_const_long(insns, dest, *c),
        SsaOp::Unop { jvm_op, a } => {
            let dop = match jvm_op {
                // negation: neg-int/long/float/double
                0x74 => 0x7b, 0x75 => 0x7d, 0x76 => 0x7f, 0x77 => 0x80,
                // conversions (i2l..i2s) → DEX 0x81..0x8f (12x, A=dest, B=src)
                0x85 => 0x81, 0x86 => 0x82, 0x87 => 0x83, 0x88 => 0x84, 0x89 => 0x85,
                0x8a => 0x86, 0x8b => 0x87, 0x8c => 0x88, 0x8d => 0x89, 0x8e => 0x8a,
                0x8f => 0x8b, 0x90 => 0x8c, 0x91 => 0x8d, 0x92 => 0x8e, 0x93 => 0x8f,
                other => bail!("ssa dexbuilder: unop {other:#x} unsupported"),
            };
            insns.push(dop | (nib(dest)? << 8) | (nib(reg(*a))? << 12));
        }
        SsaOp::Cmp { jvm_op, a, b } => {
            let (dop, _) = crate::bootstrap::cmp_op(*jvm_op);
            insns.push(dop | (dest << 8));
            insns.push((reg(*a) & 0xff) | ((reg(*b) & 0xff) << 8));
        }
        SsaOp::Binop { jvm_op, a, b, .. } => emit_binop(f, insns, alloc, dest, *jvm_op, *a, *b, frame_hint)?,
        // Invokes/field-accesses are emitted by `emit_invoke`/`emit_field` (they
        // carry extra state); the rest define no emittable instruction on their own.
        SsaOp::Phi { .. }
        | SsaOp::Argument { .. }
        | SsaOp::ConstString { .. }
        | SsaOp::ConstClass { .. }
        | SsaOp::Invoke { .. }
        | SsaOp::GetField { .. }
        | SsaOp::GetStatic { .. }
        | SsaOp::PutField { .. }
        | SsaOp::PutStatic { .. }
        | SsaOp::ArrayGet { .. }
        | SsaOp::ArrayPut { .. }
        | SsaOp::ArrayLength { .. }
        | SsaOp::NewInstance { .. }
        | SsaOp::NewArray { .. }
        | SsaOp::CheckCast { .. }
        | SsaOp::InstanceOf { .. }
        | SsaOp::Monitor { .. }
        | SsaOp::CaughtException => {
            bail!("ssa dexbuilder: value {v} has no emittable instruction")
        }
    }
    Ok(())
}

fn emit_binop(f: &SsaFn, insns: &mut Vec<u16>, alloc: &Allocation, dest: u16, jvm_op: u8, a: ValId, b: ValId, frame_hint: Option<u16>) -> Result<()> {
    let reg = |x: ValId| alloc.reg[x as usize];
    // The compact /2addr form encodes both registers in 4-bit nibbles. Emit writes ALLOCATED
    // registers and `remap_insns` later remaps them args-high — so the choice of /2addr vs the
    // 3-address (8-bit) form must be made against the FINAL register, not the allocated one.
    // Compute it here (the remap is the identity for no-pressure methods). This estimate ignores
    // scratch registers (range-invoke/φ-cycle/switch temps not yet accumulated at emit time),
    // which can only push ARG registers HIGHER — so when it underestimates we may pick /2addr and
    // `remap_insns` bails (never truncates). The form choice is purely a bail-vs-succeed lever:
    // BOTH forms are semantically identical, so this can never cause a miscompile.
    let num_arg = f.num_arg_registers;
    // The true frame on a scratch-aware retry (else the scratch-free size); only ever UNDER-counts
    // without the hint, so a missed case bails in remap rather than miscompiling.
    let regs_used = frame_hint.unwrap_or(alloc.registers_used.max(num_arg));
    // /2addr is usable for a register only when BOTH its allocated number (so the nibble emit
    // doesn't truncate it before remap reads it) AND its FINAL number (so remap doesn't overflow
    // the nibble) are ≤15. Locals remap DOWN (final = allocated - num_arg ≤ allocated), so for them
    // the allocated check implies the final; args remap UP (pushed high), so for them the final
    // check is the binding one. Requiring both is correct for every operand.
    let fits_nibble = |r: u16| -> bool {
        if r > 15 {
            return false;
        }
        let fr = if num_arg == 0 || regs_used == num_arg {
            r
        } else {
            crate::regalloc::remap_register(r, num_arg, regs_used)
        };
        fr <= 15
    };
    // Lit-fold when the right operand is a rematerialized small constant. `x - c`
    // has no DEX lit form, so d8 folds it as `x + (-c)` (iadd lit op, negated const).
    if is_rematerialized(f, b) {
        if let SsaOp::ConstInt(c) = f.values[b as usize].op {
            let (fold_op, fold_c) = if jvm_op == 0x64 { (0x60u8, -c) } else { (jvm_op, c) };
            if let Some((op8, op16)) = crate::bootstrap::lit_ops(fold_op) {
                if (-128..=127).contains(&fold_c) {
                    insns.push(op8 | (dest << 8));
                    insns.push((reg(a) & 0xff) | (((fold_c as u16) & 0xff) << 8));
                    return Ok(());
                } else {
                    insns.push(op16 | (nib(dest)? << 8) | (nib(reg(a))? << 12));
                    insns.push(fold_c as u16);
                    return Ok(());
                }
            }
            // Shift by a constant: `shl/shr/ushr-int/lit8` (22b). No lit16 form; the
            // amount fits a byte (guaranteed by is_rematerialized). The const is the
            // shift amount, never negated.
            if let Some(op8) = crate::bootstrap::shift_lit8_op(jvm_op) {
                insns.push(op8 | (dest << 8));
                insns.push((reg(a) & 0xff) | (((c as u16) & 0xff) << 8));
                return Ok(());
            }
        }
    }
    // Lit-fold a COMMUTATIVE op whose LEFT operand is the rematerialized const: d8 folds
    // `3*n` as `mul-int/lit8 n, #3` (the variable `b` is the source, the const the
    // literal). Only int commutative ops have a lit form; others fall through to 3-addr.
    if crate::bootstrap::is_commutative(jvm_op) && !is_rematerialized(f, b) && is_rematerialized(f, a)
    {
        if let SsaOp::ConstInt(c) = f.values[a as usize].op {
            if let Some((op8, op16)) = crate::bootstrap::lit_ops(jvm_op) {
                if (-128..=127).contains(&c) {
                    insns.push(op8 | (dest << 8));
                    insns.push((reg(b) & 0xff) | (((c as u16) & 0xff) << 8));
                    return Ok(());
                } else {
                    insns.push(op16 | (nib(dest)? << 8) | (nib(reg(b))? << 12));
                    insns.push(c as u16);
                    return Ok(());
                }
            }
        }
    }
    // `c - x` (const minus variable): DEX's reverse-subtract `rsub-int/lit8 x,#c` (0xd9,
    // 22b) or `rsub-int/lit16 x,#c` (0xd1, 22s). (Plain sub has no const form; `x - c`
    // folds as add-neg above.)
    if jvm_op == 0x64 && !is_rematerialized(f, b) && is_rematerialized(f, a) {
        if let SsaOp::ConstInt(c) = f.values[a as usize].op {
            if (-128..=127).contains(&c) {
                insns.push(0xd9 | (dest << 8));
                insns.push((reg(b) & 0xff) | (((c as u16) & 0xff) << 8));
                return Ok(());
            } else {
                insns.push(0xd1 | (nib(dest)? << 8) | (nib(reg(b))? << 12));
                insns.push(c as u16);
                return Ok(());
            }
        }
    }
    let (ra, rb) = (reg(a), reg(b));
    let mul_bug = is_mul_bug_min_api() && crate::bootstrap::is_mul_op(jvm_op);
    // The compact /2addr (12x) form encodes both registers in 4-bit nibbles, so it's only
    // usable when both fit (≤15). When a register is ≥16 (a >16-register method) fall through
    // to the 3-address form below, whose 8-bit fields carry the value correctly through remap
    // (remap_insns then bails-not-truncates if even the byte overflows). For ≤16-register
    // methods every register is <16, so /2addr is still chosen — byte-identical to before.
    if let Some(op2) = crate::bootstrap::binop_2addr_op(jvm_op) {
        if !mul_bug && dest == ra && fits_nibble(dest) && fits_nibble(rb) {
            insns.push(op2 | ((dest as u16) << 8) | ((rb as u16) << 12));
            return Ok(());
        }
        if !mul_bug && crate::bootstrap::is_commutative(jvm_op) && dest == rb && fits_nibble(dest) && fits_nibble(ra) {
            insns.push(op2 | ((dest as u16) << 8) | ((ra as u16) << 12));
            return Ok(());
        }
    }
    // 3-address form. d8 keeps source order when the dest already matches the LEFT
    // source (`p*i` with p=v1,i=v0 → `mul-int v1, v1, v0`), but for a COMMUTATIVE op
    // whose result reuses the RIGHT operand it swaps so the reused (dest) register is
    // the LEFT source — `x*i` with x=v5 live, i=v3 dying → `mul-long v3, v3, v5`, not
    // `mul-long v3, v5, v3` (mirrors the 2addr `op/2addr vDest, vOther`; reached for
    // the mul-2addr-bug 3-addr form below API 23).
    let op3 = crate::bootstrap::binop_3addr_op(jvm_op)?;
    let (bb, cc) = if crate::bootstrap::is_commutative(jvm_op) && dest == rb && dest != ra {
        (rb, ra)
    } else {
        (ra, rb)
    };
    insns.push(op3 | (dest << 8));
    insns.push((bb & 0xff) | ((cc & 0xff) << 8));
    Ok(())
}

/// The IR path currently targets min-api 1, where the mul-2addr bug applies.
fn is_mul_bug_min_api() -> bool {
    true
}

fn emit_const_int(insns: &mut Vec<u16>, reg: u16, c: i32, num_arg: u16, est: u16) {
    // const/4 (11n) packs the dest in a 4-bit nibble; only use it when the register fits — BOTH
    // its allocated number (else the nibble emit truncates before remap) AND its FINAL args-high
    // number (a const can be allocated to a dead-argument register that remaps high). A register
    // ≥16 falls to const/16 (21s, 8-bit AA), which covers the same small constants. ≤16-register
    // methods keep const/4 — byte-identical to before.
    let fits_nibble = reg <= 15
        && crate::regalloc::remap_register(reg, num_arg, est.max(num_arg).max(1)) <= 15;
    if (-8..=7).contains(&c) && fits_nibble {
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
    fn reserve_scratch_shifts_locals_keeps_args() {
        // num_arg = 2 (allocated 0,1 are args). Locals at allocated 2,3,4; a rematerialized
        // const at NO_REG. Reserve k=2: args unchanged, locals += 2, NO_REG untouched, frame +2.
        let mut a = Allocation { reg: vec![0, 1, 2, 3, 4, NO_REG], registers_used: 5 };
        reserve_scratch(&mut a, 2, 2);
        assert_eq!(a.reg, vec![0, 1, 4, 5, 6, NO_REG]);
        assert_eq!(a.registers_used, 7);
        // The freed allocated slots [num_arg, num_arg+k) = [2,4) now hold no value → after the
        // args-high remap they become the lowest final registers, the ≤15 scratch a spill uses.
        assert_eq!(crate::regalloc::remap_register(2, 2, 7), 0);
        assert_eq!(crate::regalloc::remap_register(3, 2, 7), 1);
        // k=0 is the identity.
        let mut b = Allocation { reg: vec![0, 1, 2], registers_used: 3 };
        reserve_scratch(&mut b, 1, 0);
        assert_eq!((b.reg, b.registers_used), (vec![0, 1, 2], 3));
    }

    #[test]
    fn count_loop_phi() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures/Loop.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == "count").unwrap();
        let bc = &m.code.as_ref().unwrap().bytecode;
        let cfg = Cfg::build(bc, &[]).unwrap();
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
        build_ssa(&cf, bc, &ps, false, &[]).unwrap()
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
        dex_method_ssa(&cf, &code.bytecode, &ps, false, &code.line_numbers, &code.exceptions).unwrap()
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
    fn clamp_now_dexes_correctly() {
        // `int x = i; if (x>5) x = 5; s += x` — the x merge-φ has the live loop counter
        // `i` as an operand. Interference-aware coalescing keeps x and i in DISTINCT
        // registers (not merged — that would clobber i), leaving a φ-move on a critical
        // edge. That move is now emitted (entry-move / fall-through inline / taken
        // trampoline) instead of bailing. Correctness PROVEN on ART by tests/art/ArtClamp
        // (clampSum(10/3/20) → 35,3,85); here we just assert it dexes (no bail).
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../skotch-dex/tests/fixtures/Clamp.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let m = cf.methods.iter().find(|m| m.name == "clampSum").unwrap();
        let c = m.code.as_ref().unwrap();
        let r = dex_method_ssa(&cf, &c.bytecode, &["I".to_string()], false, &c.line_numbers, &c.exceptions);
        assert!(r.is_ok(), "clampSum should dex now (φ-move on a critical edge): {:#}", r.err().unwrap());
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
        let code = match dex_method_ssa(&cf, &c.bytecode, &["I".to_string()], false, &c.line_numbers, &c.exceptions) {
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
        // Nested loop. The SSA path BUILDS it correctly and produces VALID, MORE-OPTIMAL
        // code (5 registers) that DIVERGES from d8: d8 leaves a dead `const/4 v0` (an
        // un-DCE'd undefined-φ-entry materialization → 6 registers) we don't reproduce.
        // We now DEX nested loops (functional-correctness; ART-validated via ArtNested) —
        // this diagnostic just confirms the (acceptable, smaller) byte divergence from d8.
        let expected = [
            0x0012, 0x0112, 0x0212, 0x5135, 0x000e, 0x0312, 0x5335, 0x0008, 0x0492, 0x0301,
            0x42b0, 0x03d8, 0x0103, 0xf928, 0x01d8, 0x0101, 0xf328, 0x020f,
        ];
        assert!(!diag("grid", &expected, 6), "grid: expected (acceptable) divergence from d8's dead-const shape");
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
