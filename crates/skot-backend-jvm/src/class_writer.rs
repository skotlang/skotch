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
use skot_config::jvm;
use skot_intern::Interner;
use skot_mir::{
    BasicBlock, BinOp as MBinOp, CallKind, LocalId, MirConst, MirFunction, MirModule, Rvalue, Stmt,
    Terminator,
};
use skot_types::Ty;
use std::io::Write;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_STATIC: u16 = 0x0008;
const ACC_FINAL: u16 = 0x0010;
const ACC_SUPER: u16 = 0x0020;

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

    // Slot 0 of `main` is reserved for `String[] args` whether or not
    // we read it. We don't model `args` in the MIR.
    if func.name == "main" {
        next_slot = 1;
    } else {
        for &p in &func.params {
            slots.insert(p.0, next_slot);
            next_slot += 1;
        }
    }

    let block: &BasicBlock = &func.blocks[0];
    walk_block(
        block,
        cp,
        module,
        func,
        class_name,
        &mut code,
        &mut stack,
        &mut max_stack,
        &mut slots,
        &mut next_slot,
    );

    match &block.terminator {
        Terminator::Return => code.push(0xB1),
        Terminator::ReturnValue(_) => code.push(0xB1), // PR #1 only voids
    }

    let max_locals = next_slot as u16;

    let mut code_attr = Vec::<u8>::new();
    code_attr
        .write_u16::<BigEndian>(max_stack.max(0) as u16)
        .unwrap();
    code_attr.write_u16::<BigEndian>(max_locals.max(1)).unwrap();
    code_attr.write_u32::<BigEndian>(code.len() as u32).unwrap();
    code_attr.write_all(&code).unwrap();
    code_attr.write_u16::<BigEndian>(0).unwrap(); // exception_table_length
    code_attr.write_u16::<BigEndian>(0).unwrap(); // attributes_count

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
    cp: &mut ConstantPool,
    module: &MirModule,
    func: &MirFunction,
    class_name: &str,
    code: &mut Vec<u8>,
    stack: &mut i32,
    max_stack: &mut i32,
    slots: &mut FxHashMap<u32, u8>,
    next_slot: &mut u8,
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
                let opcode: u8 = match op {
                    MBinOp::AddI => 0x60,
                    MBinOp::SubI => 0x64,
                    MBinOp::MulI => 0x68,
                    MBinOp::DivI => 0x6C,
                    MBinOp::ModI => 0x70,
                };
                code.push(opcode);
                bump(stack, max_stack, -1);
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
                            Ty::Int | Ty::Bool => "(I)V",
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
                    bump(stack, max_stack, -(args.len() as i32));
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

#[cfg(test)]
mod tests {
    use super::*;
    use skot_intern::Interner;
    use skot_lexer::lex;
    use skot_mir_lower::lower_file;
    use skot_parser::parse_file;
    use skot_resolve::resolve_file;
    use skot_span::FileId;
    use skot_typeck::type_check;

    fn compile(src: &str) -> (Vec<(String, Vec<u8>)>, skot_diagnostics::Diagnostics) {
        let mut interner = Interner::new();
        let mut diags = skot_diagnostics::Diagnostics::new();
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
