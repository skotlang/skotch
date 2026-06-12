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
pub fn dex_class(cf: &ClassFile) -> Result<ClassDef> {
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
        let em = dex_method(cf, m)?;
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

fn dex_method(cf: &ClassFile, m: &Member) -> Result<EncodedMethod> {
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
        Some(translate_code(cf, m, c, &params, &ret)?)
    } else {
        None
    };
    Ok(EncodedMethod { method, access_flags: access, code })
}

/// A lazily-tracked operand-stack value.
#[derive(Clone)]
enum Val {
    Local(u16),
    #[allow(dead_code)] // value carried for the Phase-1 IR; bootstrap rejects it
    Const(i32),
}

fn translate_code(
    cf: &ClassFile,
    m: &Member,
    code: &Code,
    params: &[String],
    ret: &str,
) -> Result<CodeItem> {
    let instance = m.access_flags & 0x0008 == 0;
    let ins_size = arg_register_count(params, instance);
    // Bootstrap restriction: every local is an incoming argument (no temps).
    if code.max_locals as u32 != ins_size {
        bail!(
            "method {}{} needs register allocation (max_locals {} != ins {}); requires Phase 1 IR",
            m.name,
            m.descriptor,
            code.max_locals,
            ins_size
        );
    }
    let registers_size = code.max_locals;

    let mut out = Emitter::new(cf);
    let mut stack: Vec<Val> = Vec::new();
    let bc = &code.bytecode;
    let mut i = 0;
    while i < bc.len() {
        let op = bc[i];
        match op {
            // aload_0..3 / iload_0..3 / fload / lload / dload (lazy local ref)
            0x1a..=0x1d => { stack.push(Val::Local((op - 0x1a) as u16)); i += 1; } // iload_n
            0x22..=0x25 => { stack.push(Val::Local((op - 0x22) as u16)); i += 1; } // fload_n
            0x2a..=0x2d => { stack.push(Val::Local((op - 0x2a) as u16)); i += 1; } // aload_n
            0x15 | 0x17 | 0x19 => { stack.push(Val::Local(bc[i + 1] as u16)); i += 2; } // iload/fload/aload
            // constants
            0x02..=0x08 => { stack.push(Val::Const(op as i32 - 0x03)); i += 1; } // iconst_m1..5
            0x10 => { stack.push(Val::Const(bc[i + 1] as i8 as i32)); i += 2; } // bipush
            0x11 => {
                let v = i16::from_be_bytes([bc[i + 1], bc[i + 2]]) as i32;
                stack.push(Val::Const(v));
                i += 3;
            }
            // int binops (2addr form, matching d8)
            0x60 | 0x64 | 0x68 | 0x7e | 0x80 | 0x82 => {
                let b = stack.pop().unwrap();
                let a = stack.pop().unwrap();
                let dest = out.binop_2addr(op, &a, &b)?;
                stack.push(Val::Local(dest));
                i += 1;
            }
            // invokes
            0xb6 | 0xb7 | 0xb8 | 0xb9 => {
                let idx = u16::from_be_bytes([bc[i + 1], bc[i + 2]]);
                let advance = if op == 0xb9 { 5 } else { 3 };
                let has_result = out.invoke(op, idx, &mut stack)?;
                let _ = has_result;
                i += advance;
            }
            // returns
            0xb1 => { out.return_void(); i += 1; } // return
            0xac | 0xb0 => { out.return_value(stack.pop().unwrap()); i += 1; } // ireturn/areturn
            0xad | 0xae | 0xaf => { out.return_wide(stack.pop().unwrap()); i += 1; } // lreturn/dreturn... (wide)
            other => bail!(
                "bootstrap dexer: unsupported JVM opcode {:#04x} in {}{} (needs Phase 1 IR)",
                other,
                m.name,
                m.descriptor
            ),
        }
    }

    let outs_size = out.max_outs;
    let debug_info = build_debug_info(code, params, instance, ret);

    Ok(CodeItem {
        registers_size,
        ins_size: ins_size as u16,
        outs_size,
        insns: out.insns,
        fixups: out.fixups,
        tries: vec![],
        debug_info,
    })
}

struct Emitter<'a> {
    cf: &'a ClassFile,
    insns: Vec<u16>,
    fixups: Vec<Fixup>,
    max_outs: u16,
}

impl<'a> Emitter<'a> {
    fn new(cf: &'a ClassFile) -> Emitter<'a> {
        Emitter { cf, insns: Vec::new(), fixups: Vec::new(), max_outs: 0 }
    }

    fn reg_of(&self, v: &Val) -> Result<u16> {
        match v {
            Val::Local(r) => Ok(*r),
            Val::Const(_) => bail!("bootstrap: constant materialization needs a temp register (Phase 1)"),
        }
    }

    fn binop_2addr(&mut self, jvm_op: u8, a: &Val, b: &Val) -> Result<u16> {
        // d8 emits the /2addr form (dest == first operand) for straight-line
        // int arithmetic where the first operand register is reusable.
        let dest = self.reg_of(a)?;
        let src = self.reg_of(b)?;
        let dex_op: u16 = match jvm_op {
            0x60 => 0xb0, // add-int/2addr
            0x64 => 0xb1, // sub-int/2addr
            0x68 => 0xb2, // mul-int/2addr
            0x7e => 0xb5, // and-int/2addr
            0x80 => 0xb6, // or-int/2addr
            0x82 => 0xb7, // xor-int/2addr
            _ => bail!("unsupported binop {jvm_op:#x}"),
        };
        // 12x format: op | (B<<12) | (A<<8) where A=dest, B=src.
        self.insns.push(dex_op | ((dest as u16) << 8) | ((src as u16) << 12));
        Ok(dest)
    }

    fn invoke(&mut self, jvm_op: u8, idx: u16, stack: &mut Vec<Val>) -> Result<bool> {
        let (class, name, desc) = self.cf.constant_pool.member_ref(idx)?;
        let (params, ret) = parse_descriptor(&desc)?;
        let instance = jvm_op != 0xb8; // not invokestatic
        let argc = arg_register_count(&params, instance) as usize;
        // Pop args (reverse), collect their registers.
        let mut regs: Vec<u16> = Vec::with_capacity(argc);
        let mut popped = Vec::new();
        for _ in 0..argc {
            popped.push(stack.pop().unwrap());
        }
        popped.reverse();
        for v in &popped {
            regs.push(self.reg_of(v)?);
        }
        if regs.len() > 5 || regs.iter().any(|&r| r > 15) {
            bail!("bootstrap: invoke needs range form / register move (Phase 1)");
        }
        let dex_op: u16 = match jvm_op {
            0xb6 => 0x6e, // invoke-virtual
            0xb7 => if name == "<init>" { 0x70 } else { 0x6f }, // invoke-direct / invoke-super
            0xb8 => 0x71, // invoke-static
            0xb9 => 0x74, // invoke-interface
            _ => bail!("bad invoke op"),
        };
        let a = regs.len() as u16;
        let g = if regs.len() == 5 { regs[4] } else { 0 };
        self.insns.push(dex_op | (((a << 4) | g) << 8));
        let method_unit = self.insns.len();
        self.insns.push(0); // method idx (fixup)
        // register nibbles C..F
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
        // push result (bootstrap: only void/none supported as a producer)
        if ret != "V" {
            bail!("bootstrap: non-void invoke result needs move-result + temp (Phase 1)");
        }
        Ok(ret != "V")
    }

    fn return_void(&mut self) {
        self.insns.push(0x000e);
    }
    fn return_value(&mut self, v: Val) {
        let r = match v {
            Val::Local(r) => r,
            Val::Const(_) => 0,
        };
        // return vAA (format 11x): op=0x0f, AA=reg.
        self.insns.push(0x0f | ((r) << 8));
    }
    fn return_wide(&mut self, v: Val) {
        let r = match v {
            Val::Local(r) => r,
            Val::Const(_) => 0,
        };
        self.insns.push(0x10 | ((r) << 8)); // return-wide
    }
}

/// Builds debug info matching d8's release-mode line-only output for a method
/// whose locals are all arguments (no local-variable tracking).
fn build_debug_info(code: &Code, params: &[String], _instance: bool, _ret: &str) -> Option<DebugInfo> {
    if code.line_numbers.is_empty() {
        return None;
    }
    // debug_info parameters_size counts the method's declared parameters only —
    // the implicit `this` of an instance method is not a debug parameter.
    let param_count = params.len();
    let mut lines = code.line_numbers.clone();
    lines.sort_by_key(|(pc, _)| *pc);
    let line_start = lines[0].1 as u32;
    let mut events = Vec::new();
    let mut cur_addr: i64 = 0;
    let mut cur_line: i64 = line_start as i64;
    for (pc, line) in &lines {
        let addr_diff = *pc as i64 - cur_addr;
        let line_diff = *line as i64 - cur_line;
        emit_position(&mut events, addr_diff, line_diff);
        cur_addr = *pc as i64;
        cur_line = *line as i64;
    }
    Some(DebugInfo {
        line_start,
        parameter_names: vec![None; param_count],
        events,
    })
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
