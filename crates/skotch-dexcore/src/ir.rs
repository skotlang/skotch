//! A small SSA-style IR for the linear-scan register allocator, mirroring r8's
//! `IRCode`/`Value`/`Instruction`/`BasicBlock`. Built from JVM bytecode by
//! abstract interpretation of the operand stack — each pushed value becomes a
//! `Value`, each operation an `Instr` whose `ins` reference operand values and
//! whose `out` is the (optional) result value.
//!
//! The IR carries NO register numbers: those are assigned later by the allocator,
//! and the final DEX instruction *form* (2addr vs 3addr, lit folding, …) is
//! chosen during materialization from the allocated registers — exactly as d8
//! does. This is what lets us reproduce d8's args-high placement and its
//! register-reuse choices that a single-pass heuristic cannot.

use anyhow::{bail, Result};
use skotch_classfile::ClassFile;

pub type ValId = u32;
pub type BlockId = usize;

#[derive(Clone, Debug)]
pub enum ValKind {
    /// An incoming method argument (pre-colored to its allocated register).
    Argument { arg_index: usize },
    ConstInt(i32),
    ConstLong(i64),
    ConstString(String),
    /// Produced by an instruction.
    Result,
}

#[derive(Clone, Debug)]
pub struct Value {
    pub id: ValId,
    pub kind: ValKind,
    pub wide: bool,
    pub is_ref: bool,
    pub def_block: BlockId,
    pub def_instr: usize,
}

#[derive(Clone, Debug)]
pub enum IrOp {
    Const,
    Binop { jvm_op: u8 },
    Unop { jvm_op: u8 },
    GetStatic { field_idx: u16 },
    PutStatic { field_idx: u16 },
    Invoke { jvm_op: u8, method_idx: u16 },
    If { jvm_op: u8, target: BlockId },
    Goto { target: BlockId },
    Return,
}

#[derive(Clone, Debug)]
pub struct Instr {
    pub op: IrOp,
    pub ins: Vec<ValId>,
    pub out: Option<ValId>,
    pub pc: usize,
    pub number: u32,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub start_pc: usize,
    pub end_pc: usize,
    pub instrs: Vec<Instr>,
    pub succ: Vec<BlockId>,
    pub preds: Vec<BlockId>,
}

pub struct IrMethod {
    pub values: Vec<Value>,
    pub blocks: Vec<Block>,
    pub num_arg_registers: u16,
    pub arg_values: Vec<ValId>,
    pub params: Vec<String>,
    pub ret: String,
}

impl IrMethod {
    pub fn value(&self, id: ValId) -> &Value {
        &self.values[id as usize]
    }
}

struct Builder<'a> {
    cf: &'a ClassFile,
    values: Vec<Value>,
}

impl<'a> Builder<'a> {
    fn new_value(&mut self, kind: ValKind, wide: bool, is_ref: bool, db: BlockId, di: usize) -> ValId {
        let id = self.values.len() as ValId;
        self.values.push(Value { id, kind, wide, is_ref, def_block: db, def_instr: di });
        id
    }
    fn wide(&self, id: ValId) -> bool {
        self.values[id as usize].wide
    }
}

/// `block_ranges`: (start_pc, end_pc, successor block indices), pre-split.
pub fn build_ir(
    cf: &ClassFile,
    bc: &[u8],
    block_ranges: &[(usize, usize, Vec<BlockId>)],
    params: &[String],
    ret: &str,
    instance: bool,
) -> Result<IrMethod> {
    let mut b = Builder { cf, values: Vec::new() };

    // Pre-create argument values.
    let mut arg_values = Vec::new();
    let mut num_arg_registers = 0u16;
    if instance {
        let id = b.new_value(ValKind::Argument { arg_index: 0 }, false, true, usize::MAX, usize::MAX);
        arg_values.push(id);
        num_arg_registers += 1;
    }
    for p in params {
        let wide = p == "J" || p == "D";
        let is_ref = p.starts_with('L') || p.starts_with('[');
        let id = b.new_value(
            ValKind::Argument { arg_index: arg_values.len() },
            wide, is_ref, usize::MAX, usize::MAX,
        );
        arg_values.push(id);
        num_arg_registers += if wide { 2 } else { 1 };
    }

    // JVM local slot -> value. Args fill their slots; stores update.
    let max_slot = bc.len() + 4;
    let mut slot_value: Vec<Option<ValId>> = vec![None; max_slot];
    {
        let mut slot = 0usize;
        for &av in &arg_values {
            slot_value[slot] = Some(av);
            slot += if b.wide(av) { 2 } else { 1 };
        }
    }

    let block_at = |pc: usize| block_ranges.iter().position(|r| r.0 == pc);
    let mut blocks: Vec<Block> = Vec::with_capacity(block_ranges.len());

    for (bi, (start, end, succ)) in block_ranges.iter().enumerate() {
        let mut instrs: Vec<Instr> = Vec::new();
        let mut stack: Vec<ValId> = Vec::new();
        let mut pc = *start;
        while pc < *end {
            let op = bc[pc];
            match op {
                // ── loads ──
                0x1a..=0x1d => { stack.push(load_slot(&slot_value, (op - 0x1a) as usize)?); pc += 1; }
                0x1e..=0x21 => { stack.push(load_slot(&slot_value, (op - 0x1e) as usize)?); pc += 1; }
                0x22..=0x25 => { stack.push(load_slot(&slot_value, (op - 0x22) as usize)?); pc += 1; }
                0x26..=0x29 => { stack.push(load_slot(&slot_value, (op - 0x26) as usize)?); pc += 1; }
                0x2a..=0x2d => { stack.push(load_slot(&slot_value, (op - 0x2a) as usize)?); pc += 1; }
                0x15 | 0x16 | 0x17 | 0x18 | 0x19 => { stack.push(load_slot(&slot_value, bc[pc + 1] as usize)?); pc += 2; }
                // ── constants ──
                0x02..=0x08 => { stack.push(b.const_int(bi, op as i32 - 0x03)); pc += 1; }
                0x09 | 0x0a => { stack.push(b.const_long(bi, (op - 0x09) as i64)); pc += 1; }
                0x0b => { stack.push(b.const_int(bi, 0)); pc += 1; }
                0x0c => { stack.push(b.const_int(bi, 0x3f80_0000u32 as i32)); pc += 1; }
                0x0d => { stack.push(b.const_int(bi, 0x4000_0000u32 as i32)); pc += 1; }
                0x0e => { stack.push(b.const_long(bi, 0)); pc += 1; }
                0x0f => { stack.push(b.const_long(bi, 0x3ff0_0000_0000_0000u64 as i64)); pc += 1; }
                0x10 => { stack.push(b.const_int(bi, bc[pc + 1] as i8 as i32)); pc += 2; }
                0x11 => { stack.push(b.const_int(bi, i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32)); pc += 3; }
                0x12 => { stack.push(b.ldc(bi, bc[pc + 1] as u16, false)?); pc += 2; }
                0x13 => { stack.push(b.ldc(bi, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), false)?); pc += 3; }
                0x14 => { stack.push(b.ldc(bi, u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]), true)?); pc += 3; }
                // ── stores (single-assignment slot binding) ──
                0x36..=0x3a => { let v = stack.pop().unwrap(); slot_value[bc[pc + 1] as usize] = Some(v); pc += 2; }
                0x3b..=0x3e => { let v = stack.pop().unwrap(); slot_value[(op - 0x3b) as usize] = Some(v); pc += 1; }
                0x3f..=0x42 => { let v = stack.pop().unwrap(); slot_value[(op - 0x3f) as usize] = Some(v); pc += 1; }
                0x43..=0x46 => { let v = stack.pop().unwrap(); slot_value[(op - 0x43) as usize] = Some(v); pc += 1; }
                0x47..=0x4a => { let v = stack.pop().unwrap(); slot_value[(op - 0x47) as usize] = Some(v); pc += 1; }
                0x4b..=0x4e => { let v = stack.pop().unwrap(); slot_value[(op - 0x4b) as usize] = Some(v); pc += 1; }
                // ── int binops (lit-foldable) ──
                0x60 | 0x64 | 0x68 | 0x6c | 0x70 | 0x7e | 0x80 | 0x82 | 0x78 | 0x7a | 0x7c
                // ── long/float/double binops ──
                | 0x61 | 0x65 | 0x69 | 0x7f | 0x81 | 0x83 | 0x79 | 0x7b | 0x7d
                | 0x62 | 0x66 | 0x6a
                | 0x63 | 0x67 | 0x6b => {
                    let rb = stack.pop().unwrap();
                    let ra = stack.pop().unwrap();
                    let wide = b.wide(ra);
                    let out = b.emit(&mut instrs, IrOp::Binop { jvm_op: op }, vec![ra, rb], wide, false, pc);
                    stack.push(out);
                    pc += 1;
                }
                // ── neg ──
                0x74 | 0x75 | 0x76 | 0x77 => {
                    let v = stack.pop().unwrap();
                    let wide = b.wide(v);
                    let out = b.emit(&mut instrs, IrOp::Unop { jvm_op: op }, vec![v], wide, false, pc);
                    stack.push(out);
                    pc += 1;
                }
                // ── numeric conversions ──
                0x85..=0x93 => {
                    let v = stack.pop().unwrap();
                    let (wide, is_ref) = (conv_result_wide(op), false);
                    let out = b.emit(&mut instrs, IrOp::Unop { jvm_op: op }, vec![v], wide, is_ref, pc);
                    stack.push(out);
                    pc += 1;
                }
                // ── static field access ──
                0xb2 => {
                    let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                    let desc = field_desc(cf, idx)?;
                    let wide = desc == "J" || desc == "D";
                    let is_ref = desc.starts_with('L') || desc.starts_with('[');
                    let out = b.emit(&mut instrs, IrOp::GetStatic { field_idx: idx }, vec![], wide, is_ref, pc);
                    stack.push(out);
                    pc += 3;
                }
                0xb3 => {
                    let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                    let v = stack.pop().unwrap();
                    b.emit_void(&mut instrs, IrOp::PutStatic { field_idx: idx }, vec![v], pc);
                    pc += 3;
                }
                // ── invokes (static/virtual/special/interface) ──
                0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                    let idx = u16::from_be_bytes([bc[pc + 1], bc[pc + 2]]);
                    let (_, _, desc) = cf.constant_pool.member_ref(idx)?;
                    let (mparams, mret) = parse_desc(&desc)?;
                    let instance_call = op != 0xb8;
                    let argc = mparams.len() + if instance_call { 1 } else { 0 };
                    let mut args = Vec::with_capacity(argc);
                    for _ in 0..argc {
                        args.push(stack.pop().unwrap());
                    }
                    args.reverse();
                    let advance = if op == 0xb9 { 5 } else { 3 };
                    if mret == "V" {
                        b.emit_void(&mut instrs, IrOp::Invoke { jvm_op: op, method_idx: idx }, args, pc);
                    } else {
                        let wide = mret == "J" || mret == "D";
                        let is_ref = mret.starts_with('L') || mret.starts_with('[');
                        let out = b.emit(&mut instrs, IrOp::Invoke { jvm_op: op, method_idx: idx }, args, wide, is_ref, pc);
                        stack.push(out);
                    }
                    pc += advance;
                }
                // ── conditional branches ──
                0x99..=0xa4 => {
                    let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                    let tb = block_at(target).ok_or_else(|| anyhow::anyhow!("ir: branch target not a leader"))?;
                    let two = (0x9f..=0xa4).contains(&op);
                    let ins = if two {
                        let r = stack.pop().unwrap();
                        let l = stack.pop().unwrap();
                        vec![l, r]
                    } else {
                        vec![stack.pop().unwrap()]
                    };
                    b.emit_void(&mut instrs, IrOp::If { jvm_op: op, target: tb }, ins, pc);
                    pc += 3;
                }
                0xa7 => {
                    let target = (pc as i32 + i16::from_be_bytes([bc[pc + 1], bc[pc + 2]]) as i32) as usize;
                    let tb = block_at(target).ok_or_else(|| anyhow::anyhow!("ir: goto target not a leader"))?;
                    b.emit_void(&mut instrs, IrOp::Goto { target: tb }, vec![], pc);
                    pc += 3;
                }
                // ── returns ──
                0xb1 => { b.emit_void(&mut instrs, IrOp::Return, vec![], pc); pc += 1; }
                0xac | 0xad | 0xae | 0xaf | 0xb0 => {
                    let v = stack.pop().unwrap();
                    b.emit_void(&mut instrs, IrOp::Return, vec![v], pc);
                    pc += 1;
                }
                _ => bail!("ir: unsupported opcode {op:#04x} (allocator subset)"),
            }
        }
        if !stack.is_empty() {
            bail!("ir: non-empty operand stack at block boundary (needs phi support)");
        }
        blocks.push(Block { start_pc: *start, end_pc: *end, instrs, succ: succ.clone(), preds: Vec::new() });
    }

    let n = blocks.len();
    for bi in 0..n {
        let succ = blocks[bi].succ.clone();
        for s in succ {
            blocks[s].preds.push(bi);
        }
    }

    Ok(IrMethod {
        values: b.values,
        blocks,
        num_arg_registers,
        arg_values,
        params: params.to_vec(),
        ret: ret.to_string(),
    })
}

impl<'a> Builder<'a> {
    fn const_int(&mut self, bi: BlockId, c: i32) -> ValId {
        self.new_value(ValKind::ConstInt(c), false, false, bi, usize::MAX)
    }
    fn const_long(&mut self, bi: BlockId, c: i64) -> ValId {
        self.new_value(ValKind::ConstLong(c), true, false, bi, usize::MAX)
    }
    fn ldc(&mut self, bi: BlockId, idx: u16, wide: bool) -> Result<ValId> {
        use skotch_classfile::constant_pool::Constant;
        Ok(match self.cf.constant_pool.get(idx) {
            Constant::Integer(v) => self.new_value(ValKind::ConstInt(*v), false, false, bi, usize::MAX),
            Constant::Float(f) => self.new_value(ValKind::ConstInt(f.to_bits() as i32), false, false, bi, usize::MAX),
            Constant::Long(v) if wide => self.new_value(ValKind::ConstLong(*v), true, false, bi, usize::MAX),
            Constant::Double(d) if wide => self.new_value(ValKind::ConstLong(d.to_bits() as i64), true, false, bi, usize::MAX),
            Constant::String { string_index } => {
                let s = self.cf.constant_pool.utf8(*string_index)?.to_string();
                self.new_value(ValKind::ConstString(s), false, true, bi, usize::MAX)
            }
            _ => bail!("ir: unsupported ldc constant"),
        })
    }
    /// Append a result-producing instruction; returns its out value.
    fn emit(&mut self, instrs: &mut Vec<Instr>, op: IrOp, ins: Vec<ValId>, wide: bool, is_ref: bool, pc: usize) -> ValId {
        let bi = instrs.first().map(|_| 0).unwrap_or(0); // def_block filled below by caller context
        let di = instrs.len();
        let out = self.new_value(ValKind::Result, wide, is_ref, bi, di);
        instrs.push(Instr { op, ins, out: Some(out), pc, number: 0 });
        out
    }
    fn emit_void(&mut self, instrs: &mut Vec<Instr>, op: IrOp, ins: Vec<ValId>, pc: usize) {
        instrs.push(Instr { op, ins, out: None, pc, number: 0 });
    }
}

fn load_slot(slot_value: &[Option<ValId>], slot: usize) -> Result<ValId> {
    slot_value
        .get(slot)
        .and_then(|x| *x)
        .ok_or_else(|| anyhow::anyhow!("ir: load of undefined local slot {slot} (cross-block local — needs phi)"))
}

fn field_desc(cf: &ClassFile, idx: u16) -> Result<String> {
    let (_, _, desc) = cf.constant_pool.member_ref(idx)?;
    Ok(desc)
}

/// Whether a numeric conversion's result is wide (long/double).
fn conv_result_wide(jvm_op: u8) -> bool {
    matches!(jvm_op, 0x85 | 0x87 | 0x8a | 0x8c | 0x8d) // i2l,i2d,l2d,f2l,f2d
}

fn parse_desc(desc: &str) -> Result<(Vec<String>, String)> {
    crate::bootstrap::parse_descriptor(desc)
}
