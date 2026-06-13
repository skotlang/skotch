//! Bootstrap CF→DEX translator (IR-less, straight-line).
//!
//! Handles the subset of methods where the operand stack can be resolved by
//! lazy local references — synthesized constructors and simple expression
//! bodies — producing byte-identical output to d8 for those. Anything outside
//! the subset (branches, exceptions, stack spills, register pressure) returns
//! an `unsupported` error: those need the full SSA IR + register allocator
//! (Phase 1) and must not be silently miscompiled.

use anyhow::{bail, Result};
use skotch_classfile::model::{ClassFile, Code, Member};
use skotch_dex::model::*;

const ACC_CONSTRUCTOR: u32 = 0x1_0000;

/// Translates a class file into a DEX [`ClassDef`].
pub fn dex_class(cf: &ClassFile, min_api: u32) -> Result<ClassDef> {
    let class_type = cf.descriptor();
    let superclass = cf
        .super_class
        .as_ref()
        .map(|s| skotch_classfile::constant_pool::internal_to_descriptor(s));
    let interfaces: Vec<String> = cf
        .interfaces
        .iter()
        .map(|i| skotch_classfile::constant_pool::internal_to_descriptor(i))
        .collect();

    let mut direct = Vec::new();
    let mut virtual_ = Vec::new();
    for m in &cf.methods {
        let em = dex_method(cf, m, min_api)?;
        if is_direct(m) {
            direct.push(em);
        } else {
            virtual_.push(em);
        }
    }
    // DEX requires methods sorted by method index within each list; the writer
    // re-derives indices but the encoded order must be ascending by index. For
    // the subset here we keep source order (the writer asserts via deltas).

    let static_fields = cf
        .fields
        .iter()
        .filter(|f| f.is_static())
        .map(|f| field(&class_type, f))
        .collect();
    let instance_fields = cf
        .fields
        .iter()
        .filter(|f| !f.is_static())
        .map(|f| field(&class_type, f))
        .collect();

    Ok(ClassDef {
        class_type,
        // DEX class access flags drop the JVM-only ACC_SUPER (0x20).
        access_flags: (cf.access_flags as u32) & !0x20,
        superclass,
        interfaces,
        source_file: cf.source_file.clone(),
        static_fields,
        instance_fields,
        direct_methods: direct,
        virtual_methods: virtual_,
        static_values: vec![],
    })
}

fn field(class_type: &str, f: &Member) -> EncodedField {
    EncodedField {
        field: FieldRef {
            class: class_type.to_string(),
            type_: f.descriptor.clone(),
            name: f.name.clone(),
        },
        access_flags: f.access_flags as u32,
    }
}

fn is_direct(m: &Member) -> bool {
    // direct = static, private, or constructor
    m.is_static() || m.access_flags & 0x0002 != 0 || m.name == "<init>" || m.name == "<clinit>"
}

fn dex_method(cf: &ClassFile, m: &Member, min_api: u32) -> Result<EncodedMethod> {
    let (params, ret) = parse_descriptor(&m.descriptor)?;
    let mut access = m.access_flags as u32;
    if m.name == "<init>" || m.name == "<clinit>" {
        access |= ACC_CONSTRUCTOR;
    }
    let method = MethodRef {
        class: cf.descriptor(),
        proto: ProtoRef { return_type: ret.clone(), params: params.clone() },
        name: m.name.clone(),
    };
    let code = if m.is_abstract() || m.is_native() {
        None
    } else if let Some(c) = &m.code {
        let instance = m.access_flags & 0x0008 == 0;
        // Methods with a back-edge (loops) MUST go through the SSA/φ + linear-scan
        // pipeline, which models d8's loop register allocation (φ-coalescing,
        // const rematerialization, d8's φ-ordering) and emits real
        // (already-remapped) DEX registers. The straight-line / CFG paths can't
        // model back-edges, so on a construct the SSA path doesn't yet handle
        // (e.g. a nested loop's undefined φ-entry) we propagate its bail loudly
        // rather than risk a miscompile via the acyclic fallback.
        let item = if crate::ssa::method_has_loop(&c.bytecode) {
            crate::ssa::dex_method_ssa(cf, &c.bytecode, &params, instance, &c.line_numbers)?
        } else {
            // Remap allocated → real DEX registers (d8's args-high placement).
            // This is the identity unless the method has register pressure beyond
            // its arguments, so it leaves no-pressure methods byte-identical.
            let mut item = translate_code(cf, m, c, &params, &ret, min_api)?;
            crate::regalloc::remap_insns(&mut item.insns, item.ins_size, item.registers_size);
            item
        };
        // The bootstrap allocator has no spilling / move scheduling, and the
        // 4-bit-register instruction forms it emits cannot encode a register
        // ≥ 16. d8 handles >16 registers with the full allocator's spill moves;
        // we can't, so bail rather than silently mask a register field.
        if item.registers_size > 16 {
            bail!(
                "dexer: {} registers needed in {}{} — >16 needs spilling (Phase 1 allocator)",
                item.registers_size,
                m.name,
                m.descriptor
            );
        }
        Some(item)
    } else {
        None
    };
    Ok(EncodedMethod { method, access_flags: access, code })
}

/// A lazily-tracked operand-stack value.
#[derive(Clone)]
enum Val {
    /// A local variable's register (lazy — no copy emitted).
    Local(u16, bool),
    /// A small int constant, materialized on use (or folded into a lit op).
    ConstInt(i32),
    /// A long constant.
    ConstLong(i64),
    /// A string constant.
    ConstString(String),
    /// A value already materialized in a register (temp result).
    Reg(u16, bool),
}

impl Val {
    fn is_wide(&self) -> bool {
        matches!(self, Val::Local(_, true) | Val::Reg(_, true) | Val::ConstLong(_))
    }
}

fn translate_code(
    cf: &ClassFile,
    m: &Member,
    code: &Code,
    params: &[String],
    _ret: &str,
    min_api: u32,
) -> Result<CodeItem> {
    let instance = m.access_flags & 0x0008 == 0;
    let ins_size = arg_register_count(params, instance) as u16;

    // Methods with control flow go through the CFG path (basic blocks + local
    // liveness + branch fixups). The straight-line path below stays for the
    // common case and never sees a branch.
    if method_has_branches(&code.bytecode) {
        return translate_code_cfg(cf, m, code, params, ins_size, min_api);
    }

    let lu = count_local_loads(&code.bytecode, code.max_locals as usize)?;
    let mut e = Emitter::new(cf, ins_size, &code.line_numbers, lu.loads.clone(), min_api);
    // Single-assignment locals: JVM slot → the register-backed Val a store bound.
    // Arg slots are absent here and load as `Val::Local` (their own register).
    let mut stored: std::collections::HashMap<u16, Val> = std::collections::HashMap::new();
    let mut stack: Vec<Val> = Vec::new();
    let bc = &code.bytecode;
    let mut pc = 0;
    while pc < bc.len() {
        e.set_pc(pc as u32);
        let op = bc[pc];
        match op {
            0x1a..=0x1d => { stack.push(load_local(&mut stored, (op - 0x1a) as u16, false)); pc += 1; } // iload_n
            0x1e..=0x21 => { stack.push(load_local(&mut stored, (op - 0x1e) as u16, true)); pc += 1; } // lload_n
            0x22..=0x25 => { stack.push(load_local(&mut stored, (op - 0x22) as u16, false)); pc += 1; } // fload_n
            0x26..=0x29 => { stack.push(load_local(&mut stored, (op - 0x26) as u16, true)); pc += 1; } // dload_n
            0x2a..=0x2d => { stack.push(load_local(&mut stored, (op - 0x2a) as u16, false)); pc += 1; } // aload_n
            0x15 | 0x17 | 0x19 => { stack.push(load_local(&mut stored, bc[pc + 1] as u16, false)); pc += 2; } // iload/fload/aload
            0x16 | 0x18 => { stack.push(load_local(&mut stored, bc[pc + 1] as u16, true)); pc += 2; } // lload/dload
            // local stores (single-assignment subset; bails otherwise)
            0x36..=0x4e => {
                let (slot, len) = store_slot(bc, pc).unwrap();
                let v = stack.pop().unwrap();
                bind_store(&mut stored, &lu, ins_size, slot as u16, v, &m.name, &m.descriptor)?;
                pc += len;
            }
            // constants
            0x02..=0x08 => { stack.push(Val::ConstInt(op as i32 - 0x03)); pc += 1; } // iconst_m1..5
            0x09 | 0x0a => { stack.push(Val::ConstLong((op - 0x09) as i64)); pc += 1; } // lconst_0/1
            // fconst_0/1/2 and dconst_0/1 push float/double bit patterns.
            0x0b => { stack.push(Val::ConstInt(0)); pc += 1; }                           // fconst_0 = 0.0f
            0x0c => { stack.push(Val::ConstInt(0x3f80_0000u32 as i32)); pc += 1; }        // fconst_1 = 1.0f
            0x0d => { stack.push(Val::ConstInt(0x4000_0000u32 as i32)); pc += 1; }        // fconst_2 = 2.0f
            0x0e => { stack.push(Val::ConstLong(0)); pc += 1; }                           // dconst_0 = 0.0
            0x0f => { stack.push(Val::ConstLong(0x3ff0_0000_0000_0000u64 as i64)); pc += 1; } // dconst_1 = 1.0
            0x10 => { stack.push(Val::ConstInt(bc[pc + 1] as i8 as i32)); pc += 2; } // bipush
            0x11 => { stack.push(Val::ConstInt(i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32)); pc += 3; } // sipush
            0x12 => { stack.push(e.ldc(cf, bc[pc + 1] as u16)?); pc += 2; } // ldc
            0x13 => { stack.push(e.ldc(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]))?); pc += 3; } // ldc_w
            0x14 => { stack.push(e.ldc2(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]))?); pc += 3; } // ldc2_w
            // int binops (lit-folding + 2addr, matching d8)
            0x60 | 0x64 | 0x68 | 0x6c | 0x70 | 0x7e | 0x80 | 0x82 | 0x78 | 0x7a | 0x7c => {
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(e.int_binop(op, a, b)?);
                pc += 1;
            }
            // long / float / double binops (add/sub/mul/div/rem, bitwise,
            // shifts). No lit-folding for these — straight to reg form. Integer
            // div/rem record a debug position (they throw); float/double don't.
            0x61 | 0x65 | 0x69 | 0x6d | 0x71 | 0x7f | 0x81 | 0x83 | 0x79 | 0x7b | 0x7d // long
            | 0x62 | 0x66 | 0x6a | 0x6e | 0x72 // float
            | 0x63 | 0x67 | 0x6b | 0x6f | 0x73 => { // double
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                stack.push(e.binop_reg(op, a, b)?);
                pc += 1;
            }
            // numeric conversions that d8 emits as `conv vDest, vSrc` reusing the
            // source's low register (i2f/i2b/i2c/i2s, l2f, f2i, d2i/d2l/d2f). The
            // widening forms (i2l/i2d/f2l/f2d) need args-high, and l2i picks the
            // source's HIGH register — both diverge, so they fall through to the
            // bail below rather than be matched here.
            0x86 | 0x91 | 0x92 | 0x93 | 0x89 | 0x8b | 0x8e | 0x8f | 0x90 => {
                let (dexop, wide_res) = conv_op(op).unwrap();
                let v = stack.pop().unwrap();
                let src = e.materialize(&v)?;
                e.release(&v);
                let dest = e.alloc_result(src, wide_res)?;
                e.emit_unary(dexop, dest, src);
                stack.push(Val::Reg(dest, wide_res));
                pc += 1;
            }
            // static field access
            0xb2 => { stack.push(e.getstatic(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]))?); pc += 3; }
            0xb3 => { let v = stack.pop().unwrap(); e.putstatic(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), v)?; pc += 3; }
            // instance field access
            0xb4 => { let o = stack.pop().unwrap(); stack.push(e.getfield(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), o)?); pc += 3; }
            0xb5 => { let v = stack.pop().unwrap(); let o = stack.pop().unwrap(); e.putfield(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), o, v)?; pc += 3; }
            // invokes
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let advance = if op == 0xb9 { 5 } else { 3 };
                if let Some(result) = e.invoke(op, idx, &mut stack)? {
                    stack.push(result);
                }
                pc += advance;
            }
            // object allocation: `new-instance` then (after dup + <init>) the
            // reference flows on. `dup` duplicates the new reference on the stack.
            0xbb => { stack.push(e.new_instance(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]))?); pc += 3; }
            0x59 => { let top = stack.last().unwrap().clone(); stack.push(top); pc += 1; }
            // arrays
            0xbe => { let a = stack.pop().unwrap(); stack.push(e.array_length(a)?); pc += 1; }
            0x2e..=0x35 => { let i = stack.pop().unwrap(); let a = stack.pop().unwrap(); stack.push(e.array_load(op, a, i)?); pc += 1; }
            0x4f..=0x56 => { let v = stack.pop().unwrap(); let i = stack.pop().unwrap(); let a = stack.pop().unwrap(); e.array_store(op, a, i, v)?; pc += 1; }
            0xbc => { let s = stack.pop().unwrap(); stack.push(e.new_array(s, newarray_desc(bc[pc + 1]).to_string())?); pc += 2; }
            // negation (int/long/float/double)
            0x74 | 0x75 | 0x76 | 0x77 => { let v = stack.pop().unwrap(); stack.push(e.negate(op, v)?); pc += 1; }
            // checkcast / instanceof
            0xc0 => { let o = stack.pop().unwrap(); stack.push(e.check_cast(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), o)?); pc += 3; }
            0xc1 => { let o = stack.pop().unwrap(); stack.push(e.instance_of(cf, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), o)?); pc += 3; }
            0xbd => {
                let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                let elem = cf.constant_pool.class_name(idx)?.to_string();
                let elem_desc = skotch_classfile::constant_pool::internal_to_descriptor(&elem);
                let s = stack.pop().unwrap();
                stack.push(e.new_array(s, format!("[{elem_desc}"))?);
                pc += 3;
            }
            // returns
            0xb1 => { e.return_void(); pc += 1; }
            0xac | 0xad | 0xae | 0xaf | 0xb0 => {
                let v = stack.pop().unwrap();
                e.return_value(v, jvm_return_dex_op(op))?;
                pc += 1;
            }
            other => bail!(
                "dexer: unsupported JVM opcode {:#04x} in {}{} (needs Phase 1 IR: branches, local stores, or register pressure)",
                other,
                m.name,
                m.descriptor
            ),
        }
    }
    if !stack.is_empty() {
        bail!("dexer: non-empty operand stack at end of {}{} (needs Phase 1 IR)", m.name, m.descriptor);
    }

    let registers_size = e.registers_size();
    let debug_info = e.build_debug_info(params);
    Ok(CodeItem {
        registers_size,
        ins_size,
        outs_size: e.max_outs,
        insns: e.insns,
        fixups: e.fixups,
        tries: vec![],
        debug_info,
    })
}

// ───────────────────────── control-flow (CFG) path ─────────────────────────
//
// Methods with branches need basic blocks + local liveness so register reuse
// matches d8 (a temp may reuse an argument's register only where that argument
// is dead). This path handles conditional branches over the arithmetic/const
// subset and bails loudly on anything that would diverge from d8: stores mixed
// with branches, goto/switch, register pressure, and d8's shared-exit
// return-merging (where two blocks returning the same register collapse to one).

/// A basic block: a half-open JVM bytecode range plus its successor block
/// indices.
pub(crate) struct Block {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) succ: Vec<usize>,
}

/// Maps a JVM conditional-branch opcode to its DEX op and whether it compares
/// two registers (`if-test`, 22t) vs one against zero (`if-testz`, 21t).
pub(crate) fn cond_branch_dex_op(jvm: u8) -> Option<(u16, bool)> {
    Some(match jvm {
        0x99 => (0x38, false), // ifeq        → if-eqz
        0x9a => (0x39, false), // ifne        → if-nez
        0x9b => (0x3a, false), // iflt        → if-ltz
        0x9c => (0x3b, false), // ifge        → if-gez
        0x9d => (0x3c, false), // ifgt        → if-gtz
        0x9e => (0x3d, false), // ifle        → if-lez
        0x9f => (0x32, true),  // if_icmpeq   → if-eq
        0xa0 => (0x33, true),  // if_icmpne   → if-ne
        0xa1 => (0x34, true),  // if_icmplt   → if-lt
        0xa2 => (0x35, true),  // if_icmpge   → if-ge
        0xa3 => (0x36, true),  // if_icmpgt   → if-gt
        0xa4 => (0x37, true),  // if_icmple   → if-le
        _ => return None,
    })
}

/// JVM instruction length, complete for the opcodes the dexer accepts plus all
/// branch/return forms (so block-boundary walks stay aligned).
pub(crate) fn jvm_step_len(bc: &[u8], pc: usize) -> usize {
    match bc[pc] {
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        0x11 | 0x13 | 0x14 | 0x84 | 0x99..=0xa7 | 0xb2..=0xb8 | 0xbb | 0xbd | 0xc0
        | 0xc1 | 0xc6 | 0xc7 => 3,
        0xc5 => 4,
        0xb9 | 0xba | 0xc8 | 0xc9 => 5,
        _ => 1,
    }
}

/// Whether a load opcode reads a wide (long/double) local.
fn load_is_wide(op: u8) -> bool {
    matches!(op, 0x16 | 0x18 | 0x1e..=0x21 | 0x26..=0x29)
}

/// The local slot a load opcode reads, or `None` if `op` is not a load.
fn load_slot_of(bc: &[u8], pc: usize) -> Option<u16> {
    let op = bc[pc];
    Some(match op {
        0x1a..=0x1d => (op - 0x1a) as u16,
        0x1e..=0x21 => (op - 0x1e) as u16,
        0x22..=0x25 => (op - 0x22) as u16,
        0x26..=0x29 => (op - 0x26) as u16,
        0x2a..=0x2d => (op - 0x2a) as u16,
        0x15..=0x19 => bc[pc + 1] as u16,
        _ => return None,
    })
}

/// True if a method contains any branch/goto/switch — i.e. needs the CFG path.
fn method_has_branches(bc: &[u8]) -> bool {
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if (0x99..=0xa8).contains(&op) || matches!(op, 0xaa | 0xab | 0xc6 | 0xc7 | 0xc8 | 0xc9) {
            return true;
        }
        pc += jvm_step_len(bc, pc);
    }
    false
}

/// The pc of the last instruction in `[start, end)`.
fn last_instr_pc(bc: &[u8], start: usize, end: usize) -> usize {
    let mut pc = start;
    let mut last = start;
    while pc < end {
        last = pc;
        pc += jvm_step_len(bc, pc);
    }
    last
}

/// A block that is exactly `load <slot>; <value-return>` — d8's canonical
/// "bare return", the shape that participates in shared-exit merging.
fn is_bare_load_return(bc: &[u8], start: usize, end: usize) -> bool {
    let first = bc[start];
    if load_slot_of(bc, start).is_none() {
        return false;
    }
    let len = if matches!(first, 0x15..=0x19) { 2 } else { 1 };
    let rpc = start + len;
    rpc < end && matches!(bc[rpc], 0xac..=0xb0) && rpc + 1 == end
}

/// Splits bytecode into basic blocks with successor edges.
pub(crate) fn split_blocks(bc: &[u8]) -> Result<Vec<Block>> {
    use std::collections::BTreeSet;
    let mut leaders: BTreeSet<usize> = BTreeSet::new();
    leaders.insert(0);
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        let len = jvm_step_len(bc, pc);
        if cond_branch_dex_op(op).is_some() || op == 0xa7 {
            let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
            leaders.insert(target);
            if pc + len < bc.len() {
                leaders.insert(pc + len);
            }
        } else if matches!(op, 0xac..=0xb1 | 0xbf) {
            if pc + len < bc.len() {
                leaders.insert(pc + len);
            }
        } else if matches!(op, 0xaa | 0xab | 0xc4 | 0xc8 | 0xc9) {
            bail!("dexer (cfg): unsupported control opcode {op:#x} (goto_w/switch/wide need Phase 1)");
        }
        pc += len;
    }
    let leaders: Vec<usize> = leaders.into_iter().collect();
    let block_at = |pc: usize| leaders.iter().position(|&l| l == pc);
    let mut blocks = Vec::with_capacity(leaders.len());
    for (i, &start) in leaders.iter().enumerate() {
        let end = if i + 1 < leaders.len() { leaders[i + 1] } else { bc.len() };
        let lpc = last_instr_pc(bc, start, end);
        let op = bc[lpc];
        let len = jvm_step_len(bc, lpc);
        let mut succ = Vec::new();
        if cond_branch_dex_op(op).is_some() {
            let target = (lpc as i32 + i16::from_be_bytes([bc[lpc + 1], bc[lpc + 2]]) as i32) as usize;
            if let Some(fb) = block_at(lpc + len) {
                succ.push(fb);
            }
            if let Some(tb) = block_at(target) {
                succ.push(tb);
            }
        } else if op == 0xa7 {
            let target = (lpc as i32 + i16::from_be_bytes([bc[lpc + 1], bc[lpc + 2]]) as i32) as usize;
            if let Some(tb) = block_at(target) {
                succ.push(tb);
            }
        } else if matches!(op, 0xac..=0xb1 | 0xbf) {
            // return / athrow — no successors
        } else if let Some(fb) = block_at(end) {
            succ.push(fb);
        }
        blocks.push(Block { start, end, succ });
    }
    Ok(blocks)
}

/// Backward dataflow for per-block live-in sets of local slots.
fn block_liveness(blocks: &[Block], bc: &[u8], _max_locals: usize) -> Vec<std::collections::BTreeSet<u16>> {
    use std::collections::BTreeSet;
    let n = blocks.len();
    let mut used: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    let mut defd: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    for (bi, blk) in blocks.iter().enumerate() {
        let mut pc = blk.start;
        let mut defined = BTreeSet::new();
        while pc < blk.end {
            if let Some(slot) = load_slot_of(bc, pc) {
                if !defined.contains(&slot) {
                    used[bi].insert(slot);
                    // A wide local occupies two registers; its high half is live
                    // too (it is never loaded on its own, so account for it here).
                    if load_is_wide(bc[pc]) {
                        used[bi].insert(slot + 1);
                    }
                }
            }
            if let Some((slot, _)) = store_slot(bc, pc) {
                defined.insert(slot as u16);
            }
            pc += jvm_step_len(bc, pc);
        }
        defd[bi] = defined;
    }
    let mut live_in: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    let mut live_out: Vec<BTreeSet<u16>> = vec![BTreeSet::new(); n];
    loop {
        let mut changed = false;
        for bi in (0..n).rev() {
            let mut lo = BTreeSet::new();
            for &s in &blocks[bi].succ {
                lo.extend(live_in[s].iter().copied());
            }
            let mut li = used[bi].clone();
            for &v in &lo {
                if !defd[bi].contains(&v) {
                    li.insert(v);
                }
            }
            if lo != live_out[bi] {
                live_out[bi] = lo;
                changed = true;
            }
            if li != live_in[bi] {
                live_in[bi] = li;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    live_in
}

/// Lowest free register (a pair if `wide`), marking it used and growing the
/// register file as needed.
fn alloc_lowest(used: &mut Vec<bool>, max_reg: &mut i32, wide: bool) -> u16 {
    let need = if wide { 2 } else { 1 };
    let mut r = 0;
    loop {
        if r + need > used.len() {
            used.resize(r + need, false);
        }
        if (0..need).all(|k| !used[r + k]) {
            for k in 0..need {
                used[r + k] = true;
            }
            *max_reg = (*max_reg).max((r + need - 1) as i32);
            return r as u16;
        }
        r += 1;
    }
}

/// Materializes a CFG-path value into a register, emitting a const if needed.
fn cfg_materialize(
    e: &mut Emitter,
    used: &mut Vec<bool>,
    max_reg: &mut i32,
    v: &Val,
) -> Result<u16> {
    match v {
        Val::Local(r, _) | Val::Reg(r, _) => Ok(*r),
        Val::ConstInt(c) => {
            let r = alloc_lowest(used, max_reg, false);
            e.emit_const_int(r, *c);
            Ok(r)
        }
        Val::ConstLong(c) => {
            let r = alloc_lowest(used, max_reg, true);
            e.emit_const_long(r, *c);
            Ok(r)
        }
        Val::ConstString(s) => {
            let r = alloc_lowest(used, max_reg, false);
            e.emit_const_string(r, s.clone());
            Ok(r)
        }
    }
}

/// One CFG emission pass's outputs (an `Emitter` plus per-block return facts).
struct CfgEmit<'a> {
    e: Emitter<'a>,
    max_reg: i32,
    ret_reg: Vec<Option<u16>>,
    bare_ret: Vec<bool>,
}

/// Emits all blocks once. Blocks in `drop_return` omit their final `return`
/// instruction (they fall through to a shared exit instead). Branch offsets are
/// patched before returning.
fn emit_cfg<'a>(
    cf: &'a ClassFile,
    code: &Code,
    ins_size: u16,
    min_api: u32,
    blocks: &[Block],
    live_in: &[std::collections::BTreeSet<u16>],
    bc: &[u8],
    loads: Vec<u32>,
    drop_return: &std::collections::HashSet<usize>,
    mname: &str,
    mdesc: &str,
) -> Result<CfgEmit<'a>> {
    let block_at = |pc: usize| blocks.iter().position(|b| b.start == pc);
    let mut e = Emitter::new(cf, ins_size, &code.line_numbers, loads, min_api);
    let mut block_unit = vec![0usize; blocks.len()];
    let mut fixups: Vec<(usize, usize)> = Vec::new();
    let mut ret_reg: Vec<Option<u16>> = vec![None; blocks.len()];
    let mut bare_ret: Vec<bool> = vec![false; blocks.len()];
    let mut max_reg: i32 = ins_size as i32 - 1;

    for (bi, blk) in blocks.iter().enumerate() {
        block_unit[bi] = e.insns.len();
        // Per-block register state: live-in locals occupy their (== slot) regs.
        let mut used = vec![false; (ins_size as usize) + 8];
        for &slot in &live_in[bi] {
            let r = slot as usize;
            if r >= used.len() {
                used.resize(r + 1, false);
            }
            used[r] = true;
        }
        let mut stack: Vec<Val> = Vec::new();
        let mut pc = blk.start;
        while pc < blk.end {
            e.set_pc(pc as u32);
            let op = bc[pc];
            match op {
                0x1a..=0x1d => { stack.push(Val::Local((op - 0x1a) as u16, false)); pc += 1; }
                0x1e..=0x21 => { stack.push(Val::Local((op - 0x1e) as u16, true)); pc += 1; }
                0x22..=0x25 => { stack.push(Val::Local((op - 0x22) as u16, false)); pc += 1; }
                0x26..=0x29 => { stack.push(Val::Local((op - 0x26) as u16, true)); pc += 1; }
                0x2a..=0x2d => { stack.push(Val::Local((op - 0x2a) as u16, false)); pc += 1; }
                0x15 | 0x17 | 0x19 => { stack.push(Val::Local(bc[pc + 1] as u16, false)); pc += 2; }
                0x16 | 0x18 => { stack.push(Val::Local(bc[pc + 1] as u16, true)); pc += 2; }
                0x02..=0x08 => { stack.push(Val::ConstInt(op as i32 - 0x03)); pc += 1; }
                0x10 => { stack.push(Val::ConstInt(bc[pc + 1] as i8 as i32)); pc += 2; }
                0x11 => { stack.push(Val::ConstInt(i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32)); pc += 3; }
                0x74 => {
                    // ineg — d8 negates in place (neg-int vR, vR).
                    let v = stack.pop().unwrap();
                    let r = cfg_materialize(&mut e, &mut used, &mut max_reg, &v)?;
                    e.emit_unary(0x7b, r, r);
                    stack.push(Val::Reg(r, false));
                    pc += 1;
                }
                // long/float/double comparison → narrow -1/0/1 result (23x).
                0x94..=0x98 => {
                    let (dex_op, wide_ops) = cmp_op(op);
                    let b = stack.pop().unwrap();
                    let a = stack.pop().unwrap();
                    let ra = cfg_materialize(&mut e, &mut used, &mut max_reg, &a)?;
                    let rb = cfg_materialize(&mut e, &mut used, &mut max_reg, &b)?;
                    // Float (narrow) cmp reuses the first operand's register; long/
                    // double (wide) cmp takes a fresh register (matching d8).
                    let dest = if wide_ops {
                        alloc_lowest(&mut used, &mut max_reg, false)
                    } else {
                        used[ra as usize] = false;
                        used[rb as usize] = false;
                        alloc_lowest(&mut used, &mut max_reg, false)
                    };
                    e.insns.push(dex_op | (dest << 8));
                    e.insns.push((ra & 0xff) | ((rb & 0xff) << 8));
                    stack.push(Val::Reg(dest, false));
                    pc += 1;
                }
                0x99..=0xa4 => {
                    let (dexop, two) = cond_branch_dex_op(op).unwrap();
                    let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                    let tb = block_at(target)
                        .ok_or_else(|| anyhow::anyhow!("dexer (cfg): branch target {target} not a block leader"))?;
                    let off_unit = if two {
                        let b = stack.pop().unwrap();
                        let a = stack.pop().unwrap();
                        let ra = cfg_materialize(&mut e, &mut used, &mut max_reg, &a)?;
                        let rb = cfg_materialize(&mut e, &mut used, &mut max_reg, &b)?;
                        e.emit_if(dexop, ra, rb)
                    } else {
                        let a = stack.pop().unwrap();
                        let ra = cfg_materialize(&mut e, &mut used, &mut max_reg, &a)?;
                        e.emit_if_z(dexop, ra)
                    };
                    fixups.push((off_unit, tb));
                    pc += 3;
                }
                0xac | 0xad | 0xae | 0xaf | 0xb0 => {
                    let v = stack.pop().unwrap();
                    let r = cfg_materialize(&mut e, &mut used, &mut max_reg, &v)?;
                    ret_reg[bi] = Some(r);
                    bare_ret[bi] = is_bare_load_return(bc, blk.start, blk.end);
                    // A merged contributor drops its return and falls through to
                    // the shared exit (the value already sits in the exit reg).
                    if !drop_return.contains(&bi) {
                        e.return_value(Val::Reg(r, v.is_wide()), jvm_return_dex_op(op))?;
                    }
                    pc += 1;
                }
                0xb1 => { e.return_void(); pc += 1; }
                other => bail!(
                    "dexer (cfg): unsupported opcode {other:#04x} in {mname}{mdesc} (needs Phase 1)"
                ),
            }
        }
        if !stack.is_empty() {
            bail!("dexer (cfg): non-empty stack at block boundary in {mname}{mdesc}");
        }
    }

    // Patch branch offsets: relative to the branch instruction's first unit.
    for (off_unit, tb) in fixups {
        let branch_start = off_unit - 1;
        let off = block_unit[tb] as i32 - branch_start as i32;
        e.insns[off_unit] = off as i16 as u16;
    }

    Ok(CfgEmit { e, max_reg, ret_reg, bare_ret })
}

fn translate_code_cfg(
    cf: &ClassFile,
    m: &Member,
    code: &Code,
    params: &[String],
    ins_size: u16,
    min_api: u32,
) -> Result<CodeItem> {
    let bc = &code.bytecode;
    let lu = count_local_loads(bc, code.max_locals as usize)?;
    if lu.stores.iter().any(|&s| s > 0) {
        bail!(
            "dexer (cfg): stores + control flow need full register allocation (Phase 1) in {}{}",
            m.name,
            m.descriptor
        );
    }
    let blocks = split_blocks(bc)?;
    let live_in = block_liveness(&blocks, bc, code.max_locals as usize);
    let no_drop = std::collections::HashSet::new();

    // Pass 1: emit without merging, to learn each block's return register.
    let pass1 = emit_cfg(
        cf, code, ins_size, min_api, &blocks, &live_in, bc, lu.loads.clone(), &no_drop, &m.name, &m.descriptor,
    )?;

    // d8 collapses a bare `return vR` shared by multiple paths into a single
    // exit block; a contributing block laid out immediately before the exit
    // drops its `return` and falls through. We support exactly that shape (one
    // contributor per exit, immediately preceding it); anything else bails.
    let mut drops: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for bi in 0..blocks.len() {
        if !pass1.bare_ret[bi] {
            continue;
        }
        let r = pass1.ret_reg[bi];
        let contributors: Vec<usize> =
            (0..blocks.len()).filter(|&bj| bj != bi && pass1.ret_reg[bj] == r).collect();
        if contributors.is_empty() {
            continue;
        }
        if contributors.len() == 1 && contributors[0] + 1 == bi && !pass1.bare_ret[contributors[0]] {
            drops.insert(contributors[0]);
        } else {
            bail!(
                "dexer (cfg): shared-exit merge shape not yet supported in {}{} (needs Phase 1)",
                m.name,
                m.descriptor
            );
        }
    }

    let emit = if drops.is_empty() {
        pass1
    } else {
        emit_cfg(
            cf, code, ins_size, min_api, &blocks, &live_in, bc, lu.loads, &drops, &m.name, &m.descriptor,
        )?
    };

    // Register pressure above the argument range is now handled by the
    // allocated→real remap in `dex_method` (d8's args-high placement): the CFG
    // path allocates in d8's "allocated space" (args at `[0, ins)`, temporaries
    // reusing dead-argument registers via liveness), and the remap moves the
    // arguments to the high registers afterward.
    let registers_size = ((emit.max_reg + 1).max(ins_size as i32)) as u16;
    let debug_info = emit.e.build_debug_info(params);
    Ok(CodeItem {
        registers_size,
        ins_size,
        outs_size: emit.e.max_outs,
        insns: emit.e.insns,
        fixups: emit.e.fixups,
        tries: vec![],
        debug_info,
    })
}

/// Straight-line DEX emitter with a register allocator matching d8 for the
/// supported subset (args in the low registers, temps reusing freed registers,
/// no register above the argument range).
struct Emitter<'a> {
    cf: &'a ClassFile,
    ins_size: u16,
    insns: Vec<u16>,
    fixups: Vec<Fixup>,
    max_outs: u16,
    /// Register occupancy (true = in use). Args pre-occupy `[0, ins)`.
    used: Vec<bool>,
    max_reg: i32,
    cur_pc: u32,
    line_numbers: Vec<(u16, u16)>,
    /// (dex_addr, line) positions recorded at throwing instructions.
    positions: Vec<(u32, u32)>,
    /// Remaining loads of each local; an argument's register is freed when its
    /// count reaches zero, so a result can coalesce into it (→ 2addr/lit).
    local_uses: Vec<u32>,
    /// Target API level. Below 23 (M), d8 avoids `mul-int/2addr` (ART bug
    /// `canHaveMul2AddrBug`) and emits the 3-address `mul-int` instead.
    min_api: u32,
}

impl<'a> Emitter<'a> {
    fn new(
        cf: &'a ClassFile,
        ins_size: u16,
        line_numbers: &[(u16, u16)],
        local_uses: Vec<u32>,
        min_api: u32,
    ) -> Emitter<'a> {
        let mut used = vec![false; ins_size as usize + 8];
        for r in 0..ins_size as usize {
            used[r] = true;
        }
        Emitter {
            cf,
            ins_size,
            insns: Vec::new(),
            fixups: Vec::new(),
            max_outs: 0,
            used,
            max_reg: ins_size as i32 - 1,
            cur_pc: 0,
            line_numbers: line_numbers.to_vec(),
            positions: Vec::new(),
            local_uses,
            min_api,
        }
    }

    fn registers_size(&self) -> u16 {
        (self.max_reg + 1).max(self.ins_size as i32) as u16
    }

    fn set_pc(&mut self, pc: u32) {
        self.cur_pc = pc;
    }

    fn cur_line(&self) -> Option<u32> {
        line_for(&self.line_numbers, self.cur_pc)
    }

    fn dex_addr(&self) -> u32 {
        self.insns.len() as u32
    }

    /// Records a position at the current DEX address (for throwing instructions).
    fn record_position(&mut self) {
        if let Some(line) = self.cur_line() {
            let addr = self.dex_addr();
            self.positions.push((addr, line));
        }
    }

    /// Allocates the lowest free register (a pair if `wide`). In a method with
    /// incoming arguments, a temp above the argument range needs d8's
    /// args-high allocation, which this bootstrap does not do — so it bails
    /// rather than miscompile.
    fn alloc(&mut self, wide: bool) -> Result<u16> {
        let need = if wide { 2 } else { 1 };
        let mut r = 0;
        loop {
            if r + need > self.used.len() {
                self.used.resize(r + need, false);
            }
            // A wide value must not occupy a pair that STRADDLES the args/locals
            // boundary: the args-high remap is consecutive within the arg region
            // and within the local region, but not across them, so `[ins-1, ins]`
            // would map to a non-consecutive real pair. Skip that start.
            if !self.straddles_args(r as u16, wide) && (0..need).all(|k| !self.used[r + k]) {
                break;
            }
            r += 1;
        }
        // Allocating above the argument range is fine: this is d8's "allocated
        // space", and the allocated→real remap in `dex_method` moves the
        // arguments to the high registers afterward (args-high placement).
        for k in 0..need {
            self.used[r + k] = true;
        }
        self.max_reg = self.max_reg.max((r + need - 1) as i32);
        Ok(r as u16)
    }

    fn free(&mut self, reg: u16, wide: bool) {
        let need = if wide { 2 } else { 1 };
        for k in 0..need {
            self.used[reg as usize + k] = false;
        }
    }

    /// Whether a wide pair starting at `r` straddles the args/locals boundary
    /// (`[ins-1, ins]`), which the args-high remap cannot keep consecutive.
    fn straddles_args(&self, r: u16, wide: bool) -> bool {
        wide && self.ins_size > 0 && r == self.ins_size - 1
    }

    /// Frees the register backing a value once it has been consumed: a temp
    /// `Reg` is freed immediately; a `Local` (argument) is freed on its last
    /// load so the next result can coalesce into it.
    fn release(&mut self, v: &Val) {
        match v {
            Val::Reg(r, w) => self.free(*r, *w),
            Val::Local(r, w) => {
                let idx = *r as usize;
                if idx < self.local_uses.len() && self.local_uses[idx] > 0 {
                    self.local_uses[idx] -= 1;
                    if self.local_uses[idx] == 0 {
                        self.free(*r, *w);
                    }
                }
            }
            _ => {}
        }
    }

    /// Ensures `v` is in a register and returns it (materializing constants).
    fn materialize(&mut self, v: &Val) -> Result<u16> {
        match v {
            Val::Local(r, _) | Val::Reg(r, _) => Ok(*r),
            Val::ConstInt(c) => {
                let r = self.alloc(false)?;
                self.emit_const_int(r, *c);
                Ok(r)
            }
            Val::ConstLong(c) => {
                let r = self.alloc(true)?;
                self.emit_const_long(r, *c);
                Ok(r)
            }
            Val::ConstString(s) => {
                let r = self.alloc(false)?;
                self.emit_const_string(r, s.clone());
                Ok(r)
            }
        }
    }

    fn emit_const_int(&mut self, reg: u16, c: i32) {
        if (-8..=7).contains(&c) {
            // const/4 (11n): op 0x12, [B(value) | A(reg)] in high byte.
            self.insns.push(0x12 | (((c as u16 & 0xf) << 4 | reg) << 8));
        } else if (-32768..=32767).contains(&c) {
            // const/16 (21s): op 0x13, AA=reg, then s16.
            self.insns.push(0x13 | (reg << 8));
            self.insns.push(c as u16);
        } else if c & 0xffff == 0 {
            // const/high16 (21h): op 0x15, AA=reg, top16.
            self.insns.push(0x15 | (reg << 8));
            self.insns.push((c >> 16) as u16);
        } else {
            // const (31i): op 0x14, AA=reg, then 32-bit.
            self.insns.push(0x14 | (reg << 8));
            self.insns.push(c as u16);
            self.insns.push((c >> 16) as u16);
        }
    }

    fn emit_const_long(&mut self, reg: u16, c: i64) {
        if (-32768..=32767).contains(&c) {
            self.insns.push(0x16 | (reg << 8)); // const-wide/16
            self.insns.push(c as u16);
        } else if (i32::MIN as i64..=i32::MAX as i64).contains(&c) {
            self.insns.push(0x17 | (reg << 8)); // const-wide/32
            self.insns.push(c as u16);
            self.insns.push((c >> 16) as u16);
        } else if c & 0xffff_ffff_ffff == 0 {
            self.insns.push(0x19 | (reg << 8)); // const-wide/high16
            self.insns.push((c >> 48) as u16);
        } else {
            self.insns.push(0x18 | (reg << 8)); // const-wide
            for k in 0..4 {
                self.insns.push((c >> (16 * k)) as u16);
            }
        }
    }

    fn emit_const_string(&mut self, reg: u16, s: String) {
        // const-string (21c): op 0x1a, AA=reg, string@BBBB.
        self.insns.push(0x1a | (reg << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::String(s), wide: false });
    }

    fn ldc(&mut self, cf: &ClassFile, idx: u16) -> Result<Val> {
        use skotch_classfile::constant_pool::Constant;
        match cf.constant_pool.get(idx) {
            Constant::Integer(v) => Ok(Val::ConstInt(*v)),
            Constant::Float(f) => Ok(Val::ConstInt(f.to_bits() as i32)),
            Constant::String { string_index } => {
                Ok(Val::ConstString(cf.constant_pool.utf8(*string_index)?.to_string()))
            }
            _ => bail!("dexer: unsupported ldc constant (needs Phase 1: class/methodhandle)"),
        }
    }

    fn ldc2(&mut self, cf: &ClassFile, idx: u16) -> Result<Val> {
        use skotch_classfile::constant_pool::Constant;
        match cf.constant_pool.get(idx) {
            Constant::Long(v) => Ok(Val::ConstLong(*v)),
            Constant::Double(d) => Ok(Val::ConstLong(d.to_bits() as i64)),
            _ => bail!("dexer: bad ldc2 constant"),
        }
    }

    fn int_binop(&mut self, jvm_op: u8, a: Val, b: Val) -> Result<Val> {
        // d8 lit-folds `x - const` as `x + (-const)`: DEX has add-int/lit but no
        // sub-int/lit (only the reverse rsub-int/lit). Rewrite isub-by-constant
        // to iadd by the negated constant so the lit folding below can apply.
        let (jvm_op, b) = match (jvm_op, &b) {
            (0x64, Val::ConstInt(c)) => (0x60, Val::ConstInt(c.wrapping_neg())),
            _ => (jvm_op, b),
        };
        // Lit-folding: `x <op> const` → the lit8/lit16 form, but only when the
        // constant fits the literal field. A larger constant falls through to the
        // register form (binop_reg), which materializes it — pre-allocating a
        // result register here would leak one, so we don't.
        if let Val::ConstInt(c) = b {
            if let Some((op8, op16)) = lit_ops(jvm_op) {
                if (-128..=127).contains(&c) {
                    let src = self.materialize(&a)?;
                    self.release(&a);
                    let dest = self.alloc_result(src, false)?;
                    self.insns.push(op8 | (dest << 8));
                    self.insns.push((src & 0xff) | (((c as u16) & 0xff) << 8));
                    return Ok(Val::Reg(dest, false));
                } else if (-32768..=32767).contains(&c) {
                    let src = self.materialize(&a)?;
                    self.release(&a);
                    let dest = self.alloc_result(src, false)?;
                    self.insns.push(op16 | ((dest as u16) << 8) | ((src as u16) << 12));
                    self.insns.push(c as u16);
                    return Ok(Val::Reg(dest, false));
                }
            }
        }
        self.binop_reg(jvm_op, a, b)
    }

    fn binop_reg(&mut self, jvm_op: u8, a: Val, b: Val) -> Result<Val> {
        // The result width follows the first operand (long/double → wide; for
        // shifts the shift-amount `b` is narrow but `a`/result are wide).
        let wide = a.is_wide();
        let need = if wide { 2 } else { 1 };
        let ra = self.materialize(&a)?;
        let rb = self.materialize(&b)?;
        self.release(&a);
        self.release(&b);
        // A constant operand was just materialized into a fresh register that the
        // binop consumes; free it so the result can coalesce into it as d8 does
        // (`(a|BIG)` → `or-int/2addr v0,v2` reuses the constant's register).
        if matches!(a, Val::ConstInt(_) | Val::ConstLong(_) | Val::ConstString(_)) {
            self.free(ra, a.is_wide());
        }
        if matches!(b, Val::ConstInt(_) | Val::ConstLong(_) | Val::ConstString(_)) {
            self.free(rb, b.is_wide());
        }
        let is_free = |e: &Self, r: u16| (0..need).all(|k| !e.used[r as usize + k]);
        let ra_free = is_free(self, ra);
        let rb_free = is_free(self, rb);
        // d8's coalescing (`Binop.isTwoAddr`): the result reuses the FIRST
        // operand's register if it is now dead, or — for a COMMUTATIVE op — the
        // second operand's register if THAT is dead (the operands are then
        // swapped, which is legal for commutative ops). Otherwise a fresh
        // register. This matches `(a+b)*(a+c)` → `add-int/2addr v1, v0` (the
        // a+b result reuses the dead `b`, since `a` is still live).
        let commutative = is_commutative(jvm_op);
        let (dest, src_for_2addr) = if ra_free {
            self.mark_reg_used(ra, wide);
            (ra, Some(rb))
        } else if commutative && rb_free {
            self.mark_reg_used(rb, wide);
            (rb, Some(ra))
        } else {
            (self.alloc(wide)?, None)
        };
        // Integer div/rem throw ArithmeticException on a zero divisor, so d8
        // records a debug position at them (a no-op when there are no line
        // numbers). idiv/irem/ldiv/lrem; float/double div/rem cannot throw.
        if matches!(jvm_op, 0x6c | 0x70 | 0x6d | 0x71) {
            self.record_position();
        }
        // Below API 23, `mul-*/2addr` triggers an ART Marshmallow bug
        // (`canHaveMul2AddrBug`), so d8 keeps the 3-address form for `mul`.
        let mul2addr_bug = self.min_api < 23 && is_mul_op(jvm_op);
        if let (Some(src), Some(op2)) = (src_for_2addr, binop_2addr_op(jvm_op)) {
            if !mul2addr_bug {
                self.insns.push(op2 | ((dest as u16) << 8) | ((src as u16) << 12));
                return Ok(Val::Reg(dest, wide));
            }
        }
        let op3 = binop_3addr_op(jvm_op)?;
        self.insns.push(op3 | (dest << 8));
        self.insns.push((ra & 0xff) | ((rb & 0xff) << 8));
        Ok(Val::Reg(dest, wide))
    }

    /// Marks a register (a pair if `wide`) used and extends `max_reg`.
    fn mark_reg_used(&mut self, reg: u16, wide: bool) {
        let need = if wide { 2 } else { 1 };
        for k in 0..need {
            self.used[reg as usize + k] = true;
        }
        self.max_reg = self.max_reg.max((reg as i32) + need as i32 - 1);
    }

    /// Picks the result register (a pair if `wide`) for a binop: reuse the first
    /// operand's register(s) if now free (→ 2addr), else allocate fresh.
    fn alloc_result(&mut self, first_operand: u16, wide: bool) -> Result<u16> {
        let need = if wide { 2 } else { 1 };
        let base = first_operand as usize;
        if !self.straddles_args(first_operand, wide) && (0..need).all(|k| !self.used[base + k]) {
            for k in 0..need {
                self.used[base + k] = true;
            }
            self.max_reg = self.max_reg.max((first_operand as i32) + need as i32 - 1);
            Ok(first_operand)
        } else {
            self.alloc(wide)
        }
    }

    fn field_op(&mut self, cf: &ClassFile, idx: u16) -> Result<(FieldRef, String)> {
        let (class, name, desc) = cf.constant_pool.member_ref(idx)?;
        Ok((
            FieldRef {
                class: skotch_classfile::constant_pool::internal_to_descriptor(&class),
                type_: desc.clone(),
                name,
            },
            desc,
        ))
    }

    fn getstatic(&mut self, cf: &ClassFile, idx: u16) -> Result<Val> {
        let (field, desc) = self.field_op(cf, idx)?;
        let wide = desc == "J" || desc == "D";
        let r = self.alloc(wide)?;
        let op = sget_op(&desc);
        self.record_position();
        self.insns.push(op | (r << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Field(field), wide: false });
        Ok(Val::Reg(r, wide))
    }

    /// `new-instance vAA, type@CCCC` (21c). The fresh reference lives in `r`
    /// until consumed; the following `dup`+`<init>` initialize it in place.
    fn new_instance(&mut self, cf: &ClassFile, idx: u16) -> Result<Val> {
        let internal = cf.constant_pool.class_name(idx)?.to_string();
        let desc = skotch_classfile::constant_pool::internal_to_descriptor(&internal);
        let r = self.alloc(false)?;
        self.record_position();
        self.insns.push(0x22 | (r << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Type(desc), wide: false });
        Ok(Val::Reg(r, false))
    }

    /// `neg-int/long/float/double vA, vB` (12x). Result reuses the operand reg.
    fn negate(&mut self, jvm_op: u8, v: Val) -> Result<Val> {
        let (dex_op, wide) = match jvm_op {
            0x74 => (0x7b, false), // ineg → neg-int
            0x75 => (0x7d, true),  // lneg → neg-long
            0x76 => (0x7f, false), // fneg → neg-float
            _ => (0x80, true),     // dneg → neg-double
        };
        let src = self.materialize(&v)?;
        self.release(&v);
        let dest = self.alloc_result(src, wide)?;
        self.insns.push(dex_op | ((dest & 0xf) << 8) | ((src & 0xf) << 12));
        Ok(Val::Reg(dest, wide))
    }

    /// `check-cast vAA, type@CCCC` (21c). Operates in place: the value stays in
    /// its register (with a narrowed type), so the same register flows on.
    fn check_cast(&mut self, cf: &ClassFile, idx: u16, obj: Val) -> Result<Val> {
        let desc = class_ref_desc(cf, idx)?;
        let r = self.materialize(&obj)?;
        self.record_position();
        self.insns.push(0x1f | (r << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Type(desc), wide: false });
        Ok(Val::Reg(r, false))
    }

    /// `instance-of vA, vB, type@CCCC` (22c). The boolean result reuses the
    /// (now dead) object register.
    fn instance_of(&mut self, cf: &ClassFile, idx: u16, obj: Val) -> Result<Val> {
        let desc = class_ref_desc(cf, idx)?;
        let rb = self.materialize(&obj)?;
        self.release(&obj);
        let dest = self.alloc_result(rb, false)?;
        self.insns.push(0x20 | ((dest & 0xf) << 8) | ((rb & 0xf) << 12));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Type(desc), wide: false });
        Ok(Val::Reg(dest, false))
    }

    /// `array-length vA, vB` (12x). The result reuses the (now dead) array reg.
    fn array_length(&mut self, arr: Val) -> Result<Val> {
        let ra = self.materialize(&arr)?;
        self.release(&arr);
        let r = self.alloc_result(ra, false)?;
        self.record_position();
        self.insns.push(0x21 | ((r & 0xf) << 8) | ((ra & 0xf) << 12));
        Ok(Val::Reg(r, false))
    }

    /// `aget* vDest, vArray, vIndex` (23x). Result reuses the array register.
    fn array_load(&mut self, jvm_op: u8, arr: Val, idx: Val) -> Result<Val> {
        let (dex_op, wide) = aget_op(jvm_op);
        let ra = self.materialize(&arr)?;
        let ri = self.materialize(&idx)?;
        self.release(&arr);
        self.release(&idx);
        if matches!(idx, Val::ConstInt(_)) {
            self.free(ri, false);
        }
        let dest = self.alloc_result(ra, wide)?;
        self.record_position();
        self.insns.push(dex_op | (dest << 8));
        self.insns.push((ra & 0xff) | ((ri & 0xff) << 8));
        Ok(Val::Reg(dest, wide))
    }

    /// `aput* vValue, vArray, vIndex` (23x). No result.
    fn array_store(&mut self, jvm_op: u8, arr: Val, idx: Val, val: Val) -> Result<()> {
        let dex_op = aput_op(jvm_op);
        let rv = self.materialize(&val)?;
        let ra = self.materialize(&arr)?;
        let ri = self.materialize(&idx)?;
        self.release(&val);
        self.release(&arr);
        self.release(&idx);
        self.record_position();
        self.insns.push(dex_op | (rv << 8));
        self.insns.push((ra & 0xff) | ((ri & 0xff) << 8));
        Ok(())
    }

    /// `new-array vA, vSize, type@CCCC` (22c). Result reuses the size register.
    fn new_array(&mut self, size: Val, array_desc: String) -> Result<Val> {
        let rs = self.materialize(&size)?;
        self.release(&size);
        let dest = self.alloc_result(rs, false)?;
        self.record_position();
        self.insns.push(0x23 | ((dest & 0xf) << 8) | ((rs & 0xf) << 12));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Type(array_desc), wide: false });
        Ok(Val::Reg(dest, false))
    }

    fn putstatic(&mut self, cf: &ClassFile, idx: u16, v: Val) -> Result<()> {
        let (field, desc) = self.field_op(cf, idx)?;
        // sput_op picks the wide variant from `desc`; the value register carries
        // its own width — no separate `wide` needed here.
        let r = self.materialize(&v)?;
        self.release(&v);
        let op = sput_op(&desc);
        self.record_position();
        self.insns.push(op | (r << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Field(field), wide: false });
        Ok(())
    }

    fn getfield(&mut self, cf: &ClassFile, idx: u16, obj: Val) -> Result<Val> {
        let (field, desc) = self.field_op(cf, idx)?;
        let wide = desc == "J" || desc == "D";
        let ro = self.materialize(&obj)?;
        // The receiver and the loaded value coexist at the `iget` (it reads the
        // object and writes the result), so d8 does NOT coalesce them: allocate
        // the result FRESH (before freeing the receiver), then release the
        // receiver. The args-high remap then places the receiver in a high
        // register and the result low (`iget v0, v1`).
        let r = self.alloc(wide)?;
        self.release(&obj);
        let op = iget_op(&desc);
        self.record_position();
        // 22c: op | (B<<12)(obj) | (A<<8)(dest), field@CCCC
        self.insns.push(op | ((r & 0xf) << 8) | ((ro & 0xf) << 12));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Field(field), wide: false });
        Ok(Val::Reg(r, wide))
    }

    fn putfield(&mut self, cf: &ClassFile, idx: u16, obj: Val, v: Val) -> Result<()> {
        let (field, desc) = self.field_op(cf, idx)?;
        let rv = self.materialize(&v)?;
        let ro = self.materialize(&obj)?;
        self.release(&v);
        self.release(&obj);
        let op = iput_op(&desc);
        self.record_position();
        self.insns.push(op | ((rv & 0xf) << 8) | ((ro & 0xf) << 12));
        let unit = self.insns.len();
        self.insns.push(0);
        self.fixups.push(Fixup { unit, item: ItemRef::Field(field), wide: false });
        Ok(())
    }

    fn invoke(&mut self, jvm_op: u8, idx: u16, stack: &mut Vec<Val>) -> Result<Option<Val>> {
        let (class, name, desc) = self.cf.constant_pool.member_ref(idx)?;
        let (params, ret) = parse_descriptor(&desc)?;
        let instance = jvm_op != 0xb8;
        let argc = params.len() + if instance { 1 } else { 0 };
        let mut popped = Vec::with_capacity(argc);
        for _ in 0..argc {
            popped.push(stack.pop().unwrap());
        }
        popped.reverse();
        // Materialize each argument into a register; wide args occupy a pair.
        let mut regs: Vec<u16> = Vec::new();
        // Registers freshly allocated to materialize constant arguments: the call
        // consumes them, so they must be freed afterward (like d8) — otherwise a
        // returned value can't reuse the dead argument register (`viaH`:
        // `const v0,#5; invoke {v0}; move-result v0`).
        let mut const_arg_regs: Vec<(u16, bool)> = Vec::new();
        for v in &popped {
            let r = self.materialize(v)?;
            regs.push(r);
            if v.is_wide() {
                regs.push(r + 1);
            }
            if matches!(v, Val::ConstInt(_) | Val::ConstLong(_) | Val::ConstString(_)) {
                const_arg_regs.push((r, v.is_wide()));
            }
        }
        // A constructor (`<init>`) initializes its receiver in place; the object
        // continues to live (in `new X; dup; <init>`, the dup'd copy is still on
        // the stack). So do NOT release the receiver of an `<init>` call — only
        // its other arguments. For all other invokes, release every operand.
        let receiver_is_pinned = instance && name == "<init>";
        for (i, v) in popped.iter().enumerate() {
            if receiver_is_pinned && i == 0 {
                continue;
            }
            self.release(v);
        }
        if regs.len() > 5 || regs.iter().any(|&r| r > 15) {
            bail!("dexer: invoke needs range form / register moves (Phase 1)");
        }
        let dex_op: u16 = match jvm_op {
            0xb6 => 0x6e,
            0xb7 => if name == "<init>" { 0x70 } else { 0x6f },
            0xb8 => 0x71,
            0xb9 => 0x74,
            _ => bail!("bad invoke op"),
        };
        self.record_position();
        let a = regs.len() as u16;
        let g = if regs.len() == 5 { regs[4] } else { 0 };
        self.insns.push(dex_op | (((a << 4) | g) << 8));
        let method_unit = self.insns.len();
        self.insns.push(0);
        let mut nib: u16 = 0;
        for (k, &r) in regs.iter().take(4).enumerate() {
            nib |= r << (4 * k);
        }
        self.insns.push(nib);
        self.fixups.push(Fixup {
            unit: method_unit,
            item: ItemRef::Method(MethodRef {
                class: skotch_classfile::constant_pool::internal_to_descriptor(&class),
                proto: ProtoRef { return_type: ret.clone(), params },
                name,
            }),
            wide: false,
        });
        self.max_outs = self.max_outs.max(a);
        // The call consumed its constant arguments; free their registers so a
        // returned value coalesces into the lowest one, as d8 does.
        for (r, w) in const_arg_regs {
            self.free(r, w);
        }
        if ret == "V" {
            Ok(None)
        } else {
            let wide = ret == "J" || ret == "D";
            let r = self.alloc(wide)?;
            // move-result/-wide/-object
            let mv: u16 = if wide { 0x0b } else if is_ref(&ret) { 0x0c } else { 0x0a };
            self.insns.push(mv | (r << 8));
            Ok(Some(Val::Reg(r, wide)))
        }
    }

    /// neg-int / not-int style 12x unary: `op vA, vB`.
    fn emit_unary(&mut self, op: u16, dest: u16, src: u16) {
        self.insns.push(op | ((dest & 0xf) << 8) | ((src & 0xf) << 12));
    }

    /// if-testz vAA, +0000 (21t) — pushes a placeholder offset; returns the
    /// unit index of the offset word so the caller can patch the branch target.
    fn emit_if_z(&mut self, op: u16, reg: u16) -> usize {
        self.insns.push(op | (reg << 8));
        let unit = self.insns.len();
        self.insns.push(0);
        unit
    }

    /// if-test vA, vB, +0000 (22t) — placeholder offset; returns its unit index.
    fn emit_if(&mut self, op: u16, a: u16, b: u16) -> usize {
        self.insns.push(op | ((a & 0xf) << 8) | ((b & 0xf) << 12));
        let unit = self.insns.len();
        self.insns.push(0);
        unit
    }

    fn return_void(&mut self) {
        self.insns.push(0x000e);
    }

    /// `dex_op` is the DEX return opcode chosen from the JVM return opcode
    /// (`return`/`return-wide`/`return-object`) — see `jvm_return_dex_op`.
    fn return_value(&mut self, v: Val, dex_op: u16) -> Result<()> {
        let r = self.materialize(&v)?;
        self.insns.push(dex_op | (r << 8));
        Ok(())
    }

    fn build_debug_info(&self, params: &[String]) -> Option<DebugInfo> {
        build_debug_info(&self.positions, params)
    }
}


/// Per-slot load and store counts. Loads free an argument's register on its last
/// use; stores let the bootstrap support single-assignment locals (it bails in
/// the translator unless a stored slot is written once and read once).
struct LocalUses {
    loads: Vec<u32>,
    stores: Vec<u32>,
}

/// Decodes a JVM local store opcode to its slot index and instruction length,
/// or `None` if `op` is not a store.
/// Whether a store opcode writes a wide (long/double) local.
pub(crate) fn store_is_wide(op: u8) -> bool {
    matches!(op, 0x37 | 0x39 | 0x3f..=0x42 | 0x47..=0x4a)
}

pub(crate) fn store_slot(bc: &[u8], pc: usize) -> Option<(usize, usize)> {
    let op = bc[pc];
    match op {
        0x36..=0x3a => Some((bc[pc + 1] as usize, 2)), // i/l/f/d/astore <idx>
        0x3b..=0x3e => Some(((op - 0x3b) as usize, 1)), // istore_0..3
        0x3f..=0x42 => Some(((op - 0x3f) as usize, 1)), // lstore_0..3
        0x43..=0x46 => Some(((op - 0x43) as usize, 1)), // fstore_0..3
        0x47..=0x4a => Some(((op - 0x47) as usize, 1)), // dstore_0..3
        0x4b..=0x4e => Some(((op - 0x4b) as usize, 1)), // astore_0..3
        _ => None,
    }
}

/// Loads a local: a stored single-assignment slot yields its bound register
/// value (consumed — the subset guarantees a single read); an argument slot
/// yields `Val::Local`, which materializes to its own register.
fn load_local(stored: &mut std::collections::HashMap<u16, Val>, slot: u16, wide: bool) -> Val {
    stored.remove(&slot).unwrap_or(Val::Local(slot, wide))
}

/// Binds a store to a local slot, restricted to the subset the bootstrap can
/// emit byte-identically without a real register allocator: a fresh (non-arg)
/// slot, written once and read once, holding a computed register value. Anything
/// else bails loudly rather than risk a register-allocation divergence from d8.
fn bind_store(
    stored: &mut std::collections::HashMap<u16, Val>,
    lu: &LocalUses,
    ins_size: u16,
    slot: u16,
    v: Val,
    mname: &str,
    mdesc: &str,
) -> Result<()> {
    let s = slot as usize;
    let stores = lu.stores.get(s).copied().unwrap_or(0);
    let loads = lu.loads.get(s).copied().unwrap_or(0);
    if slot < ins_size {
        bail!("dexer: store to argument slot {slot} in {mname}{mdesc} (needs Phase 1 register allocation)");
    }
    if stores != 1 || loads != 1 {
        bail!(
            "dexer: local slot {slot} in {mname}{mdesc} has {stores} store(s)/{loads} load(s); \
             only single-assignment single-use locals are supported (needs Phase 1)"
        );
    }
    match v {
        Val::Reg(..) => {
            stored.insert(slot, v);
            Ok(())
        }
        _ => bail!(
            "dexer: store of a non-computed value into slot {slot} in {mname}{mdesc} (needs Phase 1)"
        ),
    }
}

/// Counts loads and stores per local slot. Stores no longer bail here — the
/// translator decides per-slot whether the single-assignment subset is met.
fn count_local_loads(bc: &[u8], max_locals: usize) -> Result<LocalUses> {
    let mut loads = vec![0u32; max_locals + 1];
    let mut stores = vec![0u32; max_locals + 1];
    let mut pc = 0;
    while pc < bc.len() {
        let op = bc[pc];
        if let Some((slot, len)) = store_slot(bc, pc) {
            if slot < stores.len() {
                stores[slot] += 1;
            }
            pc += len;
            continue;
        }
        let (idx, len): (Option<usize>, usize) = match op {
            0x1a..=0x1d => (Some((op - 0x1a) as usize), 1),
            0x1e..=0x21 => (Some((op - 0x1e) as usize), 1),
            0x22..=0x25 => (Some((op - 0x22) as usize), 1),
            0x26..=0x29 => (Some((op - 0x26) as usize), 1),
            0x2a..=0x2d => (Some((op - 0x2a) as usize), 1),
            0x15 | 0x16 | 0x17 | 0x18 | 0x19 => (Some(bc[pc + 1] as usize), 2),
            _ => (None, instr_len(bc, pc)),
        };
        if let Some(i) = idx {
            if i < loads.len() {
                loads[i] += 1;
            }
        }
        pc += len;
    }
    Ok(LocalUses { loads, stores })
}

/// JVM instruction length for skipping during the use-count scan. Only needs to
/// be correct for the opcodes the bootstrap accepts; unknown ops fall to the
/// translator's loud error.
fn instr_len(bc: &[u8], pc: usize) -> usize {
    match bc[pc] {
        0x10 | 0x12 => 2,                // bipush, ldc
        0x11 | 0x13 | 0x14 => 3,         // sipush, ldc_w, ldc2_w
        0xb2..=0xb8 => 3,                // get/put field, invoke (non-interface)
        0xb9 => 5,                       // invokeinterface
        _ => 1,
    }
}

pub(crate) fn is_ref(desc: &str) -> bool {
    desc.starts_with('L') || desc.starts_with('[')
}

/// The source line of the last `LineNumberTable` entry with `start_pc <= pc`.
pub(crate) fn line_for(line_numbers: &[(u16, u16)], pc: u32) -> Option<u32> {
    let mut line = None;
    for (start, l) in line_numbers {
        if *start as u32 <= pc {
            line = Some(*l as u32);
        }
    }
    line
}

/// Builds a method's `debug_info` from positions recorded at throwing
/// instructions (d8's release shape): a position is emitted only when the line
/// changes from the last emitted one; the address state advances only on emitted
/// entries. Returns `None` when there are no positions (no debug_info_item).
pub(crate) fn build_debug_info(
    positions: &[(u32, u32)],
    params: &[String],
) -> Option<DebugInfo> {
    if positions.is_empty() {
        return None;
    }
    let mut positions = positions.to_vec();
    positions.sort_by_key(|(addr, _)| *addr);
    let line_start = positions[0].1;
    let mut events = Vec::new();
    let mut cur_addr: i64 = 0;
    let mut cur_line: i64 = line_start as i64;
    let mut first = true;
    for (addr, line) in &positions {
        if !first && *line as i64 == cur_line {
            continue;
        }
        emit_position(&mut events, *addr as i64 - cur_addr, *line as i64 - cur_line);
        cur_addr = *addr as i64;
        cur_line = *line as i64;
        first = false;
    }
    Some(DebugInfo { line_start, parameter_names: vec![None; params.len()], events })
}
/// JVM comparison opcode → (DEX `cmp*` op, operands-are-wide). The narrow
/// result is -1/0/1; `cmpl`/`cmpg` differ only in NaN handling.
pub(crate) fn cmp_op(jvm_op: u8) -> (u16, bool) {
    match jvm_op {
        0x94 => (0x31, true),  // lcmp  → cmp-long
        0x95 => (0x2d, false), // fcmpl → cmpl-float
        0x96 => (0x2e, false), // fcmpg → cmpg-float
        0x97 => (0x2f, true),  // dcmpl → cmpl-double
        _ => (0x30, true),     // dcmpg → cmpg-double
    }
}

/// JVM array-load opcode → (DEX `aget*` op, result-is-wide).
pub(crate) fn aget_op(jvm_op: u8) -> (u16, bool) {
    match jvm_op {
        0x2e | 0x30 => (0x44, false), // iaload / faload → aget
        0x2f | 0x31 => (0x45, true),  // laload / daload → aget-wide
        0x32 => (0x46, false),        // aaload → aget-object
        0x33 => (0x48, false),        // baload → aget-byte
        0x34 => (0x49, false),        // caload → aget-char
        _ => (0x4a, false),           // saload → aget-short
    }
}

/// JVM array-store opcode → DEX `aput*` op.
pub(crate) fn aput_op(jvm_op: u8) -> u16 {
    match jvm_op {
        0x4f | 0x51 => 0x4b, // iastore / fastore → aput
        0x50 | 0x52 => 0x4c, // lastore / dastore → aput-wide
        0x53 => 0x4e,        // aastore → aput-object
        0x54 => 0x4f,        // bastore → aput-byte
        0x55 => 0x50,        // castore → aput-char
        _ => 0x51,           // sastore → aput-short
    }
}

/// A CONSTANT_Class reference → type descriptor. Array classes are already
/// stored in descriptor form (`[I`); ordinary classes are internal (`a/b/C`).
fn class_ref_desc(cf: &ClassFile, idx: u16) -> Result<String> {
    let name = cf.constant_pool.class_name(idx)?;
    Ok(if name.starts_with('[') {
        name.to_string()
    } else {
        skotch_classfile::constant_pool::internal_to_descriptor(name)
    })
}

/// `newarray` atype byte → array type descriptor.
fn newarray_desc(atype: u8) -> &'static str {
    match atype {
        4 => "[Z", 5 => "[C", 6 => "[F", 7 => "[D",
        8 => "[B", 9 => "[S", 10 => "[I", _ => "[J",
    }
}

pub(crate) fn lit_ops(jvm_op: u8) -> Option<(u16, u16)> {
    // (lit8 op, lit16 op) for `x <op> const` int binops. The literal is the
    // RIGHT operand, so non-commutative div/rem fold too (`a / 7`).
    match jvm_op {
        0x60 => Some((0xd8, 0xd0)), // add
        0x68 => Some((0xda, 0xd2)), // mul
        0x6c => Some((0xdb, 0xd3)), // div
        0x70 => Some((0xdc, 0xd4)), // rem
        0x7e => Some((0xdd, 0xd5)), // and
        0x80 => Some((0xde, 0xd6)), // or
        0x82 => Some((0xdf, 0xd7)), // xor
        _ => None,
    }
}

/// JVM `imul`/`lmul`/`fmul`/`dmul` — d8's `isMul()` for the Marshmallow
/// `mul-int/2addr` workaround.
pub(crate) fn is_mul_op(jvm_op: u8) -> bool {
    matches!(jvm_op, 0x68..=0x6b)
}

/// Commutative arithmetic binops (add/mul/and/or/xor, all widths) — d8's
/// `Binop.isCommutative()`. Used to let the result coalesce into the second
/// operand's register when only that one is dead. (sub/div/rem/shifts are not.)
pub(crate) fn is_commutative(jvm_op: u8) -> bool {
    matches!(jvm_op, 0x60..=0x63 | 0x68..=0x6b | 0x7e..=0x83)
}

/// DEX return opcode for a JVM return opcode: `return-wide` for long/double,
/// `return-object` for areturn, `return` otherwise.
fn jvm_return_dex_op(jvm_op: u8) -> u16 {
    match jvm_op {
        0xad | 0xaf => 0x10, // lreturn / dreturn → return-wide
        0xb0 => 0x11,        // areturn → return-object
        _ => 0x0f,           // ireturn / freturn → return
    }
}

/// JVM numeric-conversion opcode → (DEX `conv` op, result-is-wide), for the
/// conversions d8 emits as a simple `conv vDest, vSrc` reusing the source's low
/// register. Only the byte-identical-matching subset is listed (see the match
/// arm); the rest bail.
fn conv_op(jvm: u8) -> Option<(u16, bool)> {
    Some(match jvm {
        0x86 => (0x82, false), // i2f → int-to-float
        0x91 => (0x8d, false), // i2b → int-to-byte
        0x92 => (0x8e, false), // i2c → int-to-char
        0x93 => (0x8f, false), // i2s → int-to-short
        0x89 => (0x85, false), // l2f → long-to-float
        0x8b => (0x87, false), // f2i → float-to-int
        0x8e => (0x8a, false), // d2i → double-to-int
        0x8f => (0x8b, true),  // d2l → double-to-long (wide result)
        0x90 => (0x8c, false), // d2f → double-to-float
        _ => return None,
    })
}

pub(crate) fn binop_2addr_op(jvm_op: u8) -> Option<u16> {
    Some(match jvm_op {
        // int
        0x60 => 0xb0, 0x64 => 0xb1, 0x68 => 0xb2, 0x6c => 0xb3, 0x70 => 0xb4,
        0x7e => 0xb5, 0x80 => 0xb6, 0x82 => 0xb7, 0x78 => 0xb8, 0x7a => 0xb9, 0x7c => 0xba,
        // long
        0x61 => 0xbb, 0x65 => 0xbc, 0x69 => 0xbd, 0x6d => 0xbe, 0x71 => 0xbf,
        0x7f => 0xc0, 0x81 => 0xc1, 0x83 => 0xc2, 0x79 => 0xc3, 0x7b => 0xc4, 0x7d => 0xc5,
        // float
        0x62 => 0xc6, 0x66 => 0xc7, 0x6a => 0xc8, 0x6e => 0xc9, 0x72 => 0xca,
        // double
        0x63 => 0xcb, 0x67 => 0xcc, 0x6b => 0xcd, 0x6f => 0xce, 0x73 => 0xcf,
        _ => return None,
    })
}

pub(crate) fn binop_3addr_op(jvm_op: u8) -> Result<u16> {
    Ok(match jvm_op {
        // int
        0x60 => 0x90, 0x64 => 0x91, 0x68 => 0x92, 0x6c => 0x93, 0x70 => 0x94,
        0x7e => 0x95, 0x80 => 0x96, 0x82 => 0x97, 0x78 => 0x98, 0x7a => 0x99, 0x7c => 0x9a,
        // long
        0x61 => 0x9b, 0x65 => 0x9c, 0x69 => 0x9d, 0x6d => 0x9e, 0x71 => 0x9f,
        0x7f => 0xa0, 0x81 => 0xa1, 0x83 => 0xa2, 0x79 => 0xa3, 0x7b => 0xa4, 0x7d => 0xa5,
        // float
        0x62 => 0xa6, 0x66 => 0xa7, 0x6a => 0xa8, 0x6e => 0xa9, 0x72 => 0xaa,
        // double
        0x63 => 0xab, 0x67 => 0xac, 0x6b => 0xad, 0x6f => 0xae, 0x73 => 0xaf,
        _ => bail!("unsupported binop {jvm_op:#x}"),
    })
}

pub(crate) fn sget_op(desc: &str) -> u16 {
    match desc.as_bytes()[0] {
        b'J' | b'D' => 0x61,
        b'L' | b'[' => 0x62,
        b'Z' => 0x63,
        b'B' => 0x64,
        b'C' => 0x65,
        b'S' => 0x66,
        _ => 0x60, // int/float
    }
}
pub(crate) fn sput_op(desc: &str) -> u16 {
    match desc.as_bytes()[0] {
        b'J' | b'D' => 0x68,
        b'L' | b'[' => 0x69,
        b'Z' => 0x6a,
        b'B' => 0x6b,
        b'C' => 0x6c,
        b'S' => 0x6d,
        _ => 0x67,
    }
}
pub(crate) fn iget_op(desc: &str) -> u16 {
    match desc.as_bytes()[0] {
        b'J' | b'D' => 0x53,
        b'L' | b'[' => 0x54,
        b'Z' => 0x55,
        b'B' => 0x56,
        b'C' => 0x57,
        b'S' => 0x58,
        _ => 0x52,
    }
}
pub(crate) fn iput_op(desc: &str) -> u16 {
    match desc.as_bytes()[0] {
        b'J' | b'D' => 0x5a,
        b'L' | b'[' => 0x5b,
        b'Z' => 0x5c,
        b'B' => 0x5d,
        b'C' => 0x5e,
        b'S' => 0x5f,
        _ => 0x59,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn report_battery_per_method() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skotch-dex/tests/fixtures/B.class");
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        for m in &cf.methods {
            let r = dex_method(&cf, m, 1);
            match r {
                Ok(_) => eprintln!("OK   {}{}", m.name, m.descriptor),
                Err(e) => eprintln!("FAIL {}{} :: {:#}", m.name, m.descriptor, e),
            }
        }
    }
}

const DBG_FIRST_SPECIAL: i64 = 0x0a;
const DBG_LINE_BASE: i64 = -4;
const DBG_LINE_RANGE: i64 = 15;

fn emit_position(events: &mut Vec<DebugEvent>, mut addr_diff: i64, mut line_diff: i64) {
    if line_diff < DBG_LINE_BASE || line_diff > DBG_LINE_BASE + DBG_LINE_RANGE - 1 {
        events.push(DebugEvent::AdvanceLine { line_diff: line_diff as i32 });
        line_diff = 0;
    }
    let mut adjusted = (line_diff - DBG_LINE_BASE) + DBG_LINE_RANGE * addr_diff;
    if adjusted > 0xff - DBG_FIRST_SPECIAL {
        events.push(DebugEvent::AdvancePc { addr_diff: addr_diff as u32 });
        addr_diff = 0;
        adjusted = (line_diff - DBG_LINE_BASE) + DBG_LINE_RANGE * addr_diff;
    }
    events.push(DebugEvent::Special((adjusted + DBG_FIRST_SPECIAL) as u8));
}

/// Number of argument registers (`this` + params, with long/double counting 2).
fn arg_register_count(params: &[String], instance: bool) -> u32 {
    let mut n = if instance { 1 } else { 0 };
    for p in params {
        n += if p == "J" || p == "D" { 2 } else { 1 };
    }
    n
}

/// Parses a method descriptor `(params)ret` into `(param descriptors, ret)`.
pub fn parse_descriptor(desc: &str) -> Result<(Vec<String>, String)> {
    let b = desc.as_bytes();
    if b.first() != Some(&b'(') {
        bail!("bad method descriptor {desc}");
    }
    let mut i = 1;
    let mut params = Vec::new();
    while b[i] != b')' {
        let (t, ni) = parse_type(b, i)?;
        params.push(t);
        i = ni;
    }
    i += 1; // skip ')'
    let (ret, _) = parse_type(b, i)?;
    Ok((params, ret))
}

fn parse_type(b: &[u8], mut i: usize) -> Result<(String, usize)> {
    let start = i;
    while b[i] == b'[' {
        i += 1;
    }
    match b[i] {
        b'L' => {
            while b[i] != b';' {
                i += 1;
            }
            i += 1;
        }
        b'V' | b'Z' | b'B' | b'S' | b'C' | b'I' | b'J' | b'F' | b'D' => {
            i += 1;
        }
        other => bail!("bad type char {} in descriptor", other as char),
    }
    Ok((String::from_utf8_lossy(&b[start..i]).into_owned(), i))
}
