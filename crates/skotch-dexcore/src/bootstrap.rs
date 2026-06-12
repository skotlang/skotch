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
        Some(translate_code(cf, m, c, &params, &ret, min_api)?)
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
            // returns
            0xb1 => { e.return_void(); pc += 1; }
            0xac | 0xb0 => { let v = stack.pop().unwrap(); e.return_value(v)?; pc += 1; }
            0xad | 0xae | 0xaf => { let v = stack.pop().unwrap(); e.return_value(v)?; pc += 1; }
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
        // The line of the last LineNumberTable entry with start_pc <= cur_pc.
        let mut line = None;
        for (start, l) in &self.line_numbers {
            if *start as u32 <= self.cur_pc {
                line = Some(*l as u32);
            }
        }
        line
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
            if (0..need).all(|k| !self.used[r + k]) {
                break;
            }
            r += 1;
        }
        if self.ins_size > 0 && (r as u16) >= self.ins_size {
            bail!("dexer: method needs a temporary register above the argument range (needs Phase 1 args-high allocation)");
        }
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
        // Lit-folding: `x <op> const` → the lit8/lit16 form (commutative ops),
        // matching d8.
        if let Val::ConstInt(c) = b {
            if let Some((op8, op16)) = lit_ops(jvm_op) {
                let src = self.materialize(&a)?;
                self.release(&a);
                let dest = self.alloc_result(src)?;
                if (-128..=127).contains(&c) {
                    self.insns.push(op8 | (dest << 8));
                    self.insns.push((src & 0xff) | (((c as u16) & 0xff) << 8));
                } else if (-32768..=32767).contains(&c) {
                    self.insns.push(op16 | ((dest as u16) << 8) | ((src as u16) << 12));
                    self.insns.push(c as u16);
                } else {
                    return self.binop_reg(jvm_op, a_from(dest), Val::ConstInt(c));
                }
                return Ok(Val::Reg(dest, false));
            }
        }
        self.binop_reg(jvm_op, a, b)
    }

    fn binop_reg(&mut self, jvm_op: u8, a: Val, b: Val) -> Result<Val> {
        let ra = self.materialize(&a)?;
        let rb = self.materialize(&b)?;
        self.release(&a);
        self.release(&b);
        let dest = self.alloc_result(ra)?;
        // d8 normally coalesces `dest = a op b` (dest == a) into the 2-address
        // form, EXCEPT for `mul` below API 23: `mul-int/2addr` triggers an ART
        // Marshmallow bug (`canHaveMul2AddrBug`), so d8 keeps the 3-address form.
        let mul2addr_bug = self.min_api < 23 && is_mul_op(jvm_op);
        if let Some(op2) = binop_2addr_op(jvm_op) {
            if dest == ra && !mul2addr_bug {
                self.insns.push(op2 | ((dest as u16) << 8) | ((rb as u16) << 12));
                return Ok(Val::Reg(dest, false));
            }
        }
        let op3 = binop_3addr_op(jvm_op)?;
        self.insns.push(op3 | (dest << 8));
        self.insns.push((ra & 0xff) | ((rb & 0xff) << 8));
        Ok(Val::Reg(dest, false))
    }

    /// Picks the result register for a binop: reuse the first operand's
    /// register if it is now free (→ 2addr), else allocate a fresh one.
    fn alloc_result(&mut self, first_operand: u16) -> Result<u16> {
        if !self.used[first_operand as usize] {
            self.used[first_operand as usize] = true;
            self.max_reg = self.max_reg.max(first_operand as i32);
            Ok(first_operand)
        } else {
            self.alloc(false)
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
        self.release(&obj);
        let r = self.alloc(wide)?;
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
        for v in &popped {
            let r = self.materialize(v)?;
            regs.push(r);
            if v.is_wide() {
                regs.push(r + 1);
            }
        }
        for v in &popped {
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

    fn return_void(&mut self) {
        self.insns.push(0x000e);
    }

    fn return_value(&mut self, v: Val) -> Result<()> {
        let wide = v.is_wide();
        let r = self.materialize(&v)?;
        let op: u16 = if wide {
            0x10
        } else if matches!(v, Val::Local(_, false) | Val::Reg(_, false)) && is_ref_val(&v) {
            0x11 // return-object
        } else {
            0x0f // return
        };
        self.insns.push(op | (r << 8));
        Ok(())
    }

    fn build_debug_info(&self, params: &[String]) -> Option<DebugInfo> {
        if self.positions.is_empty() {
            return None;
        }
        let mut positions = self.positions.clone();
        positions.sort_by_key(|(addr, _)| *addr);
        let line_start = positions[0].1;
        let mut events = Vec::new();
        let mut cur_addr: i64 = 0;
        let mut cur_line: i64 = line_start as i64;
        // d8 emits a debug position only when the line changes from the last
        // emitted one — two throwing instructions on the same source line (e.g.
        // `System.out.println(x)`: getstatic + invoke) get a single entry. The
        // address state advances only on emitted entries, never on skips.
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
}

fn a_from(reg: u16) -> Val {
    Val::Reg(reg, false)
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
fn store_slot(bc: &[u8], pc: usize) -> Option<(usize, usize)> {
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

fn is_ref(desc: &str) -> bool {
    desc.starts_with('L') || desc.starts_with('[')
}
fn is_ref_val(v: &Val) -> bool {
    // We don't track full types on Local/Reg; refs only matter for
    // return-object, which the bootstrap subset doesn't hit (it returns int).
    let _ = v;
    false
}

fn lit_ops(jvm_op: u8) -> Option<(u16, u16)> {
    // (lit8 op, lit16 op) for commutative-with-constant int binops.
    match jvm_op {
        0x60 => Some((0xd8, 0xd0)), // add
        0x68 => Some((0xda, 0xd2)), // mul
        0x7e => Some((0xdd, 0xd5)), // and
        0x80 => Some((0xde, 0xd6)), // or
        0x82 => Some((0xdf, 0xd7)), // xor
        _ => None,
    }
}

/// JVM `imul`/`lmul`/`fmul`/`dmul` — d8's `isMul()` for the Marshmallow
/// `mul-int/2addr` workaround.
fn is_mul_op(jvm_op: u8) -> bool {
    matches!(jvm_op, 0x68..=0x6b)
}

fn binop_2addr_op(jvm_op: u8) -> Option<u16> {
    match jvm_op {
        0x60 => Some(0xb0),
        0x64 => Some(0xb1),
        0x68 => Some(0xb2),
        0x6c => Some(0xb3),
        0x70 => Some(0xb4),
        0x7e => Some(0xb5),
        0x80 => Some(0xb6),
        0x82 => Some(0xb7),
        0x78 => Some(0xb8),
        0x7a => Some(0xb9),
        0x7c => Some(0xba),
        _ => None,
    }
}

fn binop_3addr_op(jvm_op: u8) -> Result<u16> {
    Ok(match jvm_op {
        0x60 => 0x90,
        0x64 => 0x91,
        0x68 => 0x92,
        0x6c => 0x93,
        0x70 => 0x94,
        0x7e => 0x95,
        0x80 => 0x96,
        0x82 => 0x97,
        0x78 => 0x98,
        0x7a => 0x99,
        0x7c => 0x9a,
        _ => bail!("unsupported binop {jvm_op:#x}"),
    })
}

fn sget_op(desc: &str) -> u16 {
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
fn sput_op(desc: &str) -> u16 {
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
fn iget_op(desc: &str) -> u16 {
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
fn iput_op(desc: &str) -> u16 {
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
