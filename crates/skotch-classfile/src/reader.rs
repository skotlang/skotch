//! Parses raw `.class` bytes into [`ClassFile`].

use crate::constant_pool::ConstantPool;
use crate::model::*;
use anyhow::{bail, Result};

struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Cursor<'a> {
    fn u16(&mut self) -> u16 {
        let v = u16::from_be_bytes([self.d[self.p], self.d[self.p + 1]]);
        self.p += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_be_bytes(self.d[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        v
    }
    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let v = &self.d[self.p..self.p + n];
        self.p += n;
        v
    }
}

/// Parses a `.class` file.
pub fn parse_class(data: &[u8]) -> Result<ClassFile> {
    if data.len() < 10 || data[0..4] != [0xca, 0xfe, 0xba, 0xbe] {
        bail!("not a class file (bad magic)");
    }
    let minor_version = u16::from_be_bytes([data[4], data[5]]);
    let major_version = u16::from_be_bytes([data[6], data[7]]);
    let cp_count = u16::from_be_bytes([data[8], data[9]]);
    let (cp, mut pos) = ConstantPool::parse(data, 10, cp_count)?;

    let mut c = Cursor { d: data, p: pos };
    let access_flags = c.u16();
    let this_index = c.u16();
    let super_index = c.u16();
    let this_class = cp.class_name(this_index)?.to_string();
    let super_class = if super_index == 0 {
        None
    } else {
        Some(cp.class_name(super_index)?.to_string())
    };
    let iface_count = c.u16();
    let mut interfaces = Vec::with_capacity(iface_count as usize);
    for _ in 0..iface_count {
        interfaces.push(cp.class_name(c.u16())?.to_string());
    }

    let fields = parse_members(&mut c, &cp, true)?;
    let methods = parse_members(&mut c, &cp, false)?;

    // Class attributes.
    let mut source_file = None;
    let mut bootstrap_methods = Vec::new();
    let attr_count = c.u16();
    for _ in 0..attr_count {
        let name = cp.utf8(c.u16())?.to_string();
        let len = c.u32() as usize;
        let body = c.bytes(len);
        match name.as_str() {
            "SourceFile" => {
                let idx = u16::from_be_bytes([body[0], body[1]]);
                source_file = Some(cp.utf8(idx)?.to_string());
            }
            "BootstrapMethods" => {
                bootstrap_methods = parse_bootstrap_methods(body);
            }
            _ => {}
        }
    }
    pos = c.p;
    let _ = pos;

    Ok(ClassFile {
        minor_version,
        major_version,
        constant_pool: cp,
        access_flags,
        this_class,
        super_class,
        interfaces,
        fields,
        methods,
        source_file,
        bootstrap_methods,
    })
}

fn parse_members(c: &mut Cursor, cp: &ConstantPool, is_field: bool) -> Result<Vec<Member>> {
    let count = c.u16();
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let access_flags = c.u16();
        let name = cp.utf8(c.u16())?.to_string();
        let descriptor = cp.utf8(c.u16())?.to_string();
        let attr_count = c.u16();
        let mut code = None;
        let mut constant_value = None;
        for _ in 0..attr_count {
            let aname = cp.utf8(c.u16())?.to_string();
            let len = c.u32() as usize;
            let body_start = c.p;
            match aname.as_str() {
                "Code" if !is_field => code = Some(parse_code(c.d, body_start, cp)?),
                "ConstantValue" if is_field => {
                    let idx = u16::from_be_bytes([c.d[body_start], c.d[body_start + 1]]);
                    constant_value = Some(cp.get(idx).clone());
                }
                _ => {}
            }
            c.p = body_start + len;
        }
        out.push(Member { access_flags, name, descriptor, code, constant_value });
    }
    Ok(out)
}

fn parse_code(d: &[u8], start: usize, cp: &ConstantPool) -> Result<Code> {
    let mut c = Cursor { d, p: start };
    let max_stack = c.u16();
    let max_locals = c.u16();
    let code_len = c.u32() as usize;
    let bytecode = c.bytes(code_len).to_vec();
    let exc_count = c.u16();
    let mut exceptions = Vec::with_capacity(exc_count as usize);
    for _ in 0..exc_count {
        let start_pc = c.u16();
        let end_pc = c.u16();
        let handler_pc = c.u16();
        let catch_idx = c.u16();
        let catch_type = if catch_idx == 0 {
            None
        } else {
            Some(cp.class_name(catch_idx)?.to_string())
        };
        exceptions.push(ExceptionEntry { start_pc, end_pc, handler_pc, catch_type });
    }
    let mut line_numbers = Vec::new();
    let mut local_variables = Vec::new();
    let attr_count = c.u16();
    for _ in 0..attr_count {
        let aname = cp.utf8(c.u16())?.to_string();
        let len = c.u32() as usize;
        let body_start = c.p;
        match aname.as_str() {
            "LineNumberTable" => {
                let n = u16::from_be_bytes([c.d[body_start], c.d[body_start + 1]]);
                for i in 0..n as usize {
                    let o = body_start + 2 + i * 4;
                    line_numbers.push((
                        u16::from_be_bytes([c.d[o], c.d[o + 1]]),
                        u16::from_be_bytes([c.d[o + 2], c.d[o + 3]]),
                    ));
                }
            }
            "LocalVariableTable" => {
                let n = u16::from_be_bytes([c.d[body_start], c.d[body_start + 1]]);
                for i in 0..n as usize {
                    let o = body_start + 2 + i * 10;
                    let start_pc = u16::from_be_bytes([c.d[o], c.d[o + 1]]);
                    let length = u16::from_be_bytes([c.d[o + 2], c.d[o + 3]]);
                    let name = cp.utf8(u16::from_be_bytes([c.d[o + 4], c.d[o + 5]]))?.to_string();
                    let descriptor =
                        cp.utf8(u16::from_be_bytes([c.d[o + 6], c.d[o + 7]]))?.to_string();
                    let index = u16::from_be_bytes([c.d[o + 8], c.d[o + 9]]);
                    local_variables.push(LocalVariable { start_pc, length, name, descriptor, index });
                }
            }
            _ => {}
        }
        c.p = body_start + len;
    }
    Ok(Code { max_stack, max_locals, bytecode, exceptions, line_numbers, local_variables })
}

fn parse_bootstrap_methods(body: &[u8]) -> Vec<BootstrapMethod> {
    let mut out = Vec::new();
    let n = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut o = 2;
    for _ in 0..n {
        let mh = u16::from_be_bytes([body[o], body[o + 1]]);
        let argc = u16::from_be_bytes([body[o + 2], body[o + 3]]) as usize;
        o += 4;
        let mut args = Vec::with_capacity(argc);
        for _ in 0..argc {
            args.push(u16::from_be_bytes([body[o], body[o + 1]]));
            o += 2;
        }
        out.push(BootstrapMethod { method_handle_index: mh, arguments: args });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
            .unwrap()
    }

    #[test]
    fn reads_empty_class() {
        let cf = parse_class(&fixture("Empty.class")).unwrap();
        assert_eq!(cf.this_class, "Empty");
        assert_eq!(cf.super_class.as_deref(), Some("java/lang/Object"));
        assert_eq!(cf.source_file.as_deref(), Some("Empty.java"));
        assert_eq!(cf.descriptor(), "LEmpty;");
        assert_eq!(cf.methods.len(), 1);
        let init = &cf.methods[0];
        assert_eq!(init.name, "<init>");
        assert_eq!(init.descriptor, "()V");
        let code = init.code.as_ref().unwrap();
        // aload_0 (0x2a); invokespecial (0xb7) #x; return (0xb1)
        assert_eq!(code.bytecode[0], 0x2a);
        assert_eq!(code.bytecode[1], 0xb7);
        assert_eq!(*code.bytecode.last().unwrap(), 0xb1);
        assert_eq!(code.line_numbers, vec![(0, 1)]);
    }

    #[test]
    fn reads_calc_class() {
        let cf = parse_class(&fixture("Calc.class")).unwrap();
        assert_eq!(cf.this_class, "Calc");
        let add = cf.methods.iter().find(|m| m.name == "add").unwrap();
        assert_eq!(add.descriptor, "(II)I");
        assert!(add.is_static());
        let code = add.code.as_ref().unwrap();
        // iload_0 (0x1a); iload_1 (0x1b); iadd (0x60); ireturn (0xac)
        assert_eq!(code.bytecode, vec![0x1a, 0x1b, 0x60, 0xac]);
    }
}
