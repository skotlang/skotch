//! Normalize a JVM `.class` file into a stable, human-readable text
//! form suitable for golden-file diffing.
//!
//! ## Why this exists
//!
//! `skotch` and `kotlinc` are two independent compilers, so byte-level
//! `.class` outputs differ in many cosmetic ways even when the
//! bytecode is semantically equivalent: constant pool entries are
//! laid out in different orders, debug attributes (`SourceFile`,
//! `LineNumberTable`, `LocalVariableTable`) carry source-position
//! information that is naturally different, kotlinc embeds a binary
//! `kotlin.Metadata` annotation we don't reproduce, and so on.
//!
//! [`normalize`] reads bytes in, parses the constant pool, walks each
//! method's `Code` attribute, replaces every constant-pool index with
//! a *symbolic* form (so `ldc #15` becomes `ldc "Hello, world!"`),
//! sorts methods by `name+descriptor`, and strips the noisy
//! attributes listed below. The result is a text blob that should
//! match between skotch and kotlinc for the PR #1 fixtures.
//!
//! ## Equivalence relaxations
//!
//! - **Strip:** `SourceFile`, `LineNumberTable`, `LocalVariableTable`,
//!   `LocalVariableTypeTable`, `RuntimeVisibleAnnotations` containing
//!   `Lkotlin/Metadata;`, `BootstrapMethods` if empty
//! - **Sort:** methods by `(name, descriptor)`; fields likewise
//! - **Symbolic:** constant pool indices replaced with the constants
//!   they reference
//! - **Minor version:** zeroed (kotlinc and skotch agree but other JVM
//!   compilers may differ)
//!
//! Each relaxation is implemented as a separate function and can be
//! independently disabled via [`NormalizeOptions`] for unit tests.
//!
//! ## Out of scope (for now)
//!
//! - StackMapTable normalization (PR #1 has no branches; PR #1.5 will
//!   tackle this when fixture 07 graduates).
//! - InnerClasses, EnclosingMethod, NestHost, NestMembers, PermittedSubclasses
//!   — punted until skotch can produce them.

use byteorder::{BigEndian, ReadBytesExt};
use std::fmt::Write as _;
use std::io::{Cursor, Read};

/// Knobs to selectively disable normalization passes (for unit tests).
#[derive(Clone, Debug)]
pub struct NormalizeOptions {
    pub strip_source_file: bool,
    pub strip_line_numbers: bool,
    pub strip_local_var_tables: bool,
    pub strip_kotlin_metadata: bool,
    pub sort_methods: bool,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        NormalizeOptions {
            strip_source_file: true,
            strip_line_numbers: true,
            strip_local_var_tables: true,
            strip_kotlin_metadata: true,
            sort_methods: true,
        }
    }
}

/// Result of [`normalize`] — call `.into_text()` for the diff-ready string.
pub struct Normalized {
    text: String,
}

impl Normalized {
    pub fn into_text(self) -> String {
        self.text
    }

    pub fn as_text(&self) -> &str {
        &self.text
    }
}

/// Normalize a `.class` file's bytes into a stable textual form.
///
/// Returns an error string if the input is not a valid class file
/// header. Other parse errors are recovered from where possible — the
/// goal is "best-effort diffable text", not a verifier.
pub fn normalize(bytes: &[u8], opts: &NormalizeOptions) -> Result<Normalized, String> {
    let mut p = ClassParser::new(bytes)?;
    let class = p.parse()?;
    Ok(render(&class, opts))
}

/// Convenience: normalize with default options.
pub fn normalize_default(bytes: &[u8]) -> Result<Normalized, String> {
    normalize(bytes, &NormalizeOptions::default())
}

// ─── parsed-class data structures ────────────────────────────────────────

#[derive(Clone, Debug)]
struct ClassFile {
    minor: u16,
    major: u16,
    cp: Vec<CpEntry>,
    access_flags: u16,
    this_class: u16,
    super_class: u16,
    interfaces: Vec<u16>,
    fields: Vec<MemberInfo>,
    methods: Vec<MemberInfo>,
    attributes: Vec<AttributeInfo>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum CpEntry {
    Reserved, // index 0 + the slot following Long/Double
    Utf8(String),
    Integer(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    String(u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    NameAndType(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    Dynamic(u16, u16),
    InvokeDynamic(u16, u16),
    Module(u16),
    Package(u16),
}

#[derive(Clone, Debug)]
struct MemberInfo {
    access_flags: u16,
    name_idx: u16,
    descriptor_idx: u16,
    attributes: Vec<AttributeInfo>,
}

#[derive(Clone, Debug)]
struct AttributeInfo {
    name_idx: u16,
    info: Vec<u8>,
}

// ─── class file parser ──────────────────────────────────────────────────

struct ClassParser<'a> {
    cur: Cursor<&'a [u8]>,
}

impl<'a> ClassParser<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < 10 {
            return Err("class file too short".into());
        }
        if bytes[0..4] != [0xCA, 0xFE, 0xBA, 0xBE] {
            return Err("missing 0xCAFEBABE magic".into());
        }
        Ok(ClassParser {
            cur: Cursor::new(bytes),
        })
    }

    fn parse(&mut self) -> Result<ClassFile, String> {
        let _magic = self.cur.read_u32::<BigEndian>().map_err(e)?;
        let minor = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let major = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let cp_count = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let cp = self.parse_constant_pool(cp_count)?;
        let access_flags = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let this_class = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let super_class = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let icount = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let mut interfaces = Vec::with_capacity(icount as usize);
        for _ in 0..icount {
            interfaces.push(self.cur.read_u16::<BigEndian>().map_err(e)?);
        }
        let fcount = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let mut fields = Vec::with_capacity(fcount as usize);
        for _ in 0..fcount {
            fields.push(self.parse_member()?);
        }
        let mcount = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let mut methods = Vec::with_capacity(mcount as usize);
        for _ in 0..mcount {
            methods.push(self.parse_member()?);
        }
        let acount = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let mut attributes = Vec::with_capacity(acount as usize);
        for _ in 0..acount {
            attributes.push(self.parse_attribute()?);
        }
        Ok(ClassFile {
            minor,
            major,
            cp,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields,
            methods,
            attributes,
        })
    }

    fn parse_constant_pool(&mut self, count: u16) -> Result<Vec<CpEntry>, String> {
        let mut cp: Vec<CpEntry> = Vec::with_capacity(count as usize);
        cp.push(CpEntry::Reserved); // index 0
        let mut i: u16 = 1;
        while i < count {
            let tag = self.cur.read_u8().map_err(e)?;
            let entry = match tag {
                1 => {
                    let len = self.cur.read_u16::<BigEndian>().map_err(e)? as usize;
                    let mut buf = vec![0u8; len];
                    self.cur.read_exact(&mut buf).map_err(e)?;
                    // Modified UTF-8 vs UTF-8: skotch's PR #1 only emits
                    // ASCII strings, so a lossy conversion is fine for
                    // the fixtures we ship.
                    CpEntry::Utf8(String::from_utf8_lossy(&buf).into_owned())
                }
                3 => CpEntry::Integer(self.cur.read_i32::<BigEndian>().map_err(e)?),
                4 => CpEntry::Float(self.cur.read_f32::<BigEndian>().map_err(e)?),
                5 => {
                    let v = self.cur.read_i64::<BigEndian>().map_err(e)?;
                    cp.push(CpEntry::Long(v));
                    cp.push(CpEntry::Reserved);
                    i += 2;
                    continue;
                }
                6 => {
                    let v = self.cur.read_f64::<BigEndian>().map_err(e)?;
                    cp.push(CpEntry::Double(v));
                    cp.push(CpEntry::Reserved);
                    i += 2;
                    continue;
                }
                7 => CpEntry::Class(self.cur.read_u16::<BigEndian>().map_err(e)?),
                8 => CpEntry::String(self.cur.read_u16::<BigEndian>().map_err(e)?),
                9 => CpEntry::Fieldref(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                10 => CpEntry::Methodref(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                11 => CpEntry::InterfaceMethodref(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                12 => CpEntry::NameAndType(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                15 => CpEntry::MethodHandle(
                    self.cur.read_u8().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                16 => CpEntry::MethodType(self.cur.read_u16::<BigEndian>().map_err(e)?),
                17 => CpEntry::Dynamic(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                18 => CpEntry::InvokeDynamic(
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                    self.cur.read_u16::<BigEndian>().map_err(e)?,
                ),
                19 => CpEntry::Module(self.cur.read_u16::<BigEndian>().map_err(e)?),
                20 => CpEntry::Package(self.cur.read_u16::<BigEndian>().map_err(e)?),
                other => return Err(format!("unknown constant pool tag {other}")),
            };
            cp.push(entry);
            i += 1;
        }
        Ok(cp)
    }

    fn parse_member(&mut self) -> Result<MemberInfo, String> {
        let access_flags = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let name_idx = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let descriptor_idx = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let acount = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let mut attributes = Vec::with_capacity(acount as usize);
        for _ in 0..acount {
            attributes.push(self.parse_attribute()?);
        }
        Ok(MemberInfo {
            access_flags,
            name_idx,
            descriptor_idx,
            attributes,
        })
    }

    fn parse_attribute(&mut self) -> Result<AttributeInfo, String> {
        let name_idx = self.cur.read_u16::<BigEndian>().map_err(e)?;
        let len = self.cur.read_u32::<BigEndian>().map_err(e)? as usize;
        let mut info = vec![0u8; len];
        self.cur.read_exact(&mut info).map_err(e)?;
        Ok(AttributeInfo { name_idx, info })
    }
}

fn e(err: std::io::Error) -> String {
    format!("class file parse error: {err}")
}

// ─── rendering ──────────────────────────────────────────────────────────

fn render(class: &ClassFile, opts: &NormalizeOptions) -> Normalized {
    let mut out = String::new();
    let cp = &class.cp;

    let _ = writeln!(
        out,
        "class_version major={} minor={}",
        class.major,
        if opts.strip_source_file {
            0
        } else {
            class.minor
        }
    );
    let _ = writeln!(out, "this_class    {}", cp_class_name(cp, class.this_class));
    let _ = writeln!(
        out,
        "super_class   {}",
        cp_class_name(cp, class.super_class)
    );
    let _ = writeln!(out, "access_flags  0x{:04X}", class.access_flags);

    if !class.interfaces.is_empty() {
        let names: Vec<String> = class
            .interfaces
            .iter()
            .map(|&i| cp_class_name(cp, i))
            .collect();
        let _ = writeln!(out, "interfaces    [{}]", names.join(", "));
    }

    // Fields, sorted by name+descriptor.
    let mut fields = class.fields.clone();
    if opts.sort_methods {
        fields.sort_by_key(|f| {
            (
                cp_utf8(cp, f.name_idx).to_string(),
                cp_utf8(cp, f.descriptor_idx).to_string(),
            )
        });
    }
    for f in &fields {
        let _ = writeln!(
            out,
            "field         {} : {} 0x{:04X}",
            cp_utf8(cp, f.name_idx),
            cp_utf8(cp, f.descriptor_idx),
            f.access_flags
        );
    }

    // Methods, sorted by name+descriptor.
    let mut methods = class.methods.clone();
    if opts.sort_methods {
        methods.sort_by_key(|m| {
            (
                cp_utf8(cp, m.name_idx).to_string(),
                cp_utf8(cp, m.descriptor_idx).to_string(),
            )
        });
    }
    for m in &methods {
        let name = cp_utf8(cp, m.name_idx);
        let desc = cp_utf8(cp, m.descriptor_idx);
        let _ = writeln!(
            out,
            "method        {} {} 0x{:04X}",
            name, desc, m.access_flags
        );
        for a in &m.attributes {
            let attr_name = cp_utf8(cp, a.name_idx);
            if attr_name == "Code" {
                render_code_attribute(&mut out, &a.info, cp, opts);
            } else if should_strip_attr(attr_name, opts) {
                continue;
            } else {
                let _ = writeln!(out, "  attr        {attr_name} ({} bytes)", a.info.len());
            }
        }
    }

    // Class-level attributes.
    for a in &class.attributes {
        let name = cp_utf8(cp, a.name_idx);
        if should_strip_attr(name, opts) {
            continue;
        }
        if name == "RuntimeVisibleAnnotations" && opts.strip_kotlin_metadata {
            // Kotlin embeds its Metadata as the only RVA on the class
            // for top-level files. Strip wholesale; we'll revisit if a
            // future fixture needs to inspect annotations.
            continue;
        }
        let _ = writeln!(out, "class_attr    {name} ({} bytes)", a.info.len());
    }

    Normalized { text: out }
}

fn should_strip_attr(name: &str, opts: &NormalizeOptions) -> bool {
    matches!(
        (name, opts),
        ("SourceFile", o) if o.strip_source_file
    ) || matches!(
        (name, opts),
        ("LineNumberTable", o) if o.strip_line_numbers
    ) || matches!(
        (name, opts),
        ("LocalVariableTable" | "LocalVariableTypeTable", o) if o.strip_local_var_tables
    )
}

fn render_code_attribute(out: &mut String, info: &[u8], cp: &[CpEntry], opts: &NormalizeOptions) {
    let mut cur = Cursor::new(info);
    let max_stack = cur.read_u16::<BigEndian>().unwrap_or(0);
    let max_locals = cur.read_u16::<BigEndian>().unwrap_or(0);
    let code_len = cur.read_u32::<BigEndian>().unwrap_or(0) as usize;
    let mut code_bytes = vec![0u8; code_len];
    let _ = cur.read_exact(&mut code_bytes);

    let _ = writeln!(
        out,
        "  code        max_stack={max_stack} max_locals={max_locals}"
    );
    render_bytecode(out, &code_bytes, cp);

    // Skip exception table.
    let etlen = cur.read_u16::<BigEndian>().unwrap_or(0);
    for _ in 0..etlen {
        let _ = cur.read_u16::<BigEndian>();
        let _ = cur.read_u16::<BigEndian>();
        let _ = cur.read_u16::<BigEndian>();
        let _ = cur.read_u16::<BigEndian>();
    }
    // Sub-attributes.
    let acount = cur.read_u16::<BigEndian>().unwrap_or(0);
    for _ in 0..acount {
        let name_idx = cur.read_u16::<BigEndian>().unwrap_or(0);
        let len = cur.read_u32::<BigEndian>().unwrap_or(0) as usize;
        let mut buf = vec![0u8; len];
        let _ = cur.read_exact(&mut buf);
        let name = cp_utf8(cp, name_idx);
        if should_strip_attr(name, opts) {
            continue;
        }
        let _ = writeln!(out, "    sub_attr  {name} ({} bytes)", buf.len());
    }
}

/// Disassemble bytecode into one normalized line per instruction.
/// Constant pool indices are replaced with their symbolic forms.
fn render_bytecode(out: &mut String, code: &[u8], cp: &[CpEntry]) {
    let mut i = 0;
    while i < code.len() {
        let op = code[i];
        let (mnem, span) = decode_instruction(op, &code[i..], cp, i);
        let _ = writeln!(out, "    {:04} {}", i, mnem);
        i += span;
    }
}

/// Decode one instruction. Returns `(mnemonic_with_args, length_in_bytes)`.
///
/// The table is organized by numeric opcode so each instruction's span
/// is computed correctly even when the decoder doesn't recognize it —
/// mis-sized opcodes would otherwise push the cursor into the middle of
/// the next instruction. For unknown opcodes we fall back to a single-
/// byte hex dump (`op_0xXX`). The `code_position` argument is the byte
/// offset of the opcode within the method's Code; it's currently unused
/// but provided as a hook for future `tableswitch`/`lookupswitch`
/// branch-target pretty-printing.
fn decode_instruction(
    op: u8,
    slice: &[u8],
    cp: &[CpEntry],
    code_position: usize,
) -> (String, usize) {
    match op {
        0x00 => ("nop".into(), 1),
        0x01 => ("aconst_null".into(), 1),
        0x02 => ("iconst_m1".into(), 1),
        0x03 => ("iconst_0".into(), 1),
        0x04 => ("iconst_1".into(), 1),
        0x05 => ("iconst_2".into(), 1),
        0x06 => ("iconst_3".into(), 1),
        0x07 => ("iconst_4".into(), 1),
        0x08 => ("iconst_5".into(), 1),
        0x10 if slice.len() >= 2 => (format!("bipush {}", slice[1] as i8), 2),
        0x11 if slice.len() >= 3 => {
            let v = i16::from_be_bytes([slice[1], slice[2]]);
            (format!("sipush {v}"), 3)
        }
        0x12 if slice.len() >= 2 => {
            let idx = slice[1] as u16;
            (format!("ldc {}", cp_symbolic(cp, idx)), 2)
        }
        0x13 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("ldc_w {}", cp_symbolic(cp, idx)), 3)
        }
        0x15 if slice.len() >= 2 => (format!("iload {}", slice[1]), 2),
        0x19 if slice.len() >= 2 => (format!("aload {}", slice[1]), 2),
        0x1A => ("iload_0".into(), 1),
        0x1B => ("iload_1".into(), 1),
        0x1C => ("iload_2".into(), 1),
        0x1D => ("iload_3".into(), 1),
        0x2A => ("aload_0".into(), 1),
        0x2B => ("aload_1".into(), 1),
        0x2C => ("aload_2".into(), 1),
        0x2D => ("aload_3".into(), 1),
        0x36 if slice.len() >= 2 => (format!("istore {}", slice[1]), 2),
        0x3A if slice.len() >= 2 => (format!("astore {}", slice[1]), 2),
        0x3B => ("istore_0".into(), 1),
        0x3C => ("istore_1".into(), 1),
        0x3D => ("istore_2".into(), 1),
        0x3E => ("istore_3".into(), 1),
        0x4B => ("astore_0".into(), 1),
        0x4C => ("astore_1".into(), 1),
        0x4D => ("astore_2".into(), 1),
        0x4E => ("astore_3".into(), 1),
        0x57 => ("pop".into(), 1),
        0x59 => ("dup".into(), 1),
        0x60 => ("iadd".into(), 1),
        0x64 => ("isub".into(), 1),
        0x68 => ("imul".into(), 1),
        0x6C => ("idiv".into(), 1),
        0x70 => ("irem".into(), 1),
        0x7E => ("iand".into(), 1),
        0x80 => ("ior".into(), 1),
        0x99 if slice.len() >= 3 => {
            let off = i16::from_be_bytes([slice[1], slice[2]]);
            (format!("ifeq {}", (code_position as i32) + off as i32), 3)
        }
        0x9A if slice.len() >= 3 => {
            let off = i16::from_be_bytes([slice[1], slice[2]]);
            (format!("ifne {}", (code_position as i32) + off as i32), 3)
        }
        0xA5 if slice.len() >= 3 => {
            let off = i16::from_be_bytes([slice[1], slice[2]]);
            (
                format!("if_acmpeq {}", (code_position as i32) + off as i32),
                3,
            )
        }
        0xA6 if slice.len() >= 3 => {
            let off = i16::from_be_bytes([slice[1], slice[2]]);
            (
                format!("if_acmpne {}", (code_position as i32) + off as i32),
                3,
            )
        }
        0xA7 if slice.len() >= 3 => {
            let off = i16::from_be_bytes([slice[1], slice[2]]);
            (format!("goto {}", (code_position as i32) + off as i32), 3)
        }
        0xAA => {
            // tableswitch: 1-byte opcode + 0..3 bytes padding to the
            // next 4-byte boundary + default:i32 + low:i32 + high:i32
            // + (high - low + 1) * i32 jump offsets.
            let pad = 3 - (code_position % 4);
            if slice.len() < 1 + pad + 12 {
                return (format!("op_0x{op:02X}"), 1);
            }
            let mut p = 1 + pad;
            let default = i32::from_be_bytes([slice[p], slice[p + 1], slice[p + 2], slice[p + 3]]);
            p += 4;
            let low = i32::from_be_bytes([slice[p], slice[p + 1], slice[p + 2], slice[p + 3]]);
            p += 4;
            let high = i32::from_be_bytes([slice[p], slice[p + 1], slice[p + 2], slice[p + 3]]);
            p += 4;
            let count = (high - low + 1).max(0) as usize;
            if slice.len() < p + count * 4 {
                return (format!("op_0x{op:02X}"), 1);
            }
            let mut buf = format!(
                "tableswitch default={} low={low} high={high}",
                (code_position as i32) + default
            );
            for i in 0..count {
                let off = i32::from_be_bytes([slice[p], slice[p + 1], slice[p + 2], slice[p + 3]]);
                p += 4;
                buf.push_str(&format!(
                    " {}={}",
                    low + i as i32,
                    (code_position as i32) + off
                ));
            }
            (buf, p)
        }
        0xB0 => ("areturn".into(), 1),
        0xB1 => ("return".into(), 1),
        0xB2 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("getstatic {}", cp_symbolic(cp, idx)), 3)
        }
        0xB4 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("getfield {}", cp_symbolic(cp, idx)), 3)
        }
        0xB5 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("putfield {}", cp_symbolic(cp, idx)), 3)
        }
        0xB6 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("invokevirtual {}", cp_symbolic(cp, idx)), 3)
        }
        0xB7 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("invokespecial {}", cp_symbolic(cp, idx)), 3)
        }
        0xB8 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("invokestatic {}", cp_symbolic(cp, idx)), 3)
        }
        // invokeinterface: 0xB9 <index_hi> <index_lo> <count> <0>
        0xB9 if slice.len() >= 5 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("invokeinterface {}", cp_symbolic(cp, idx)), 5)
        }
        0xBB if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("new {}", cp_symbolic(cp, idx)), 3)
        }
        0xBF => ("athrow".into(), 1),
        0xC0 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("checkcast {}", cp_symbolic(cp, idx)), 3)
        }
        0xC1 if slice.len() >= 3 => {
            let idx = u16::from_be_bytes([slice[1], slice[2]]);
            (format!("instanceof {}", cp_symbolic(cp, idx)), 3)
        }
        _ => (format!("op_0x{op:02X}"), 1),
    }
}

// ─── constant pool helpers ──────────────────────────────────────────────

fn cp_utf8(cp: &[CpEntry], idx: u16) -> &str {
    match cp.get(idx as usize) {
        Some(CpEntry::Utf8(s)) => s,
        _ => "<?>",
    }
}

fn cp_class_name(cp: &[CpEntry], idx: u16) -> String {
    match cp.get(idx as usize) {
        Some(CpEntry::Class(name_idx)) => cp_utf8(cp, *name_idx).to_string(),
        _ => "<?>".to_string(),
    }
}

fn cp_symbolic(cp: &[CpEntry], idx: u16) -> String {
    match cp.get(idx as usize) {
        Some(CpEntry::Utf8(s)) => format!("\"{s}\""),
        Some(CpEntry::Integer(v)) => format!("int({v})"),
        Some(CpEntry::Long(v)) => format!("long({v})"),
        Some(CpEntry::Float(v)) => format!("float({v})"),
        Some(CpEntry::Double(v)) => format!("double({v})"),
        Some(CpEntry::Class(n)) => format!("Class({})", cp_utf8(cp, *n)),
        Some(CpEntry::String(s)) => format!("\"{}\"", cp_utf8(cp, *s)),
        Some(CpEntry::Fieldref(c, nt)) => {
            let class = cp_class_name(cp, *c);
            let (n, d) = cp_name_and_type(cp, *nt);
            format!("Field({class}.{n}:{d})")
        }
        Some(CpEntry::Methodref(c, nt)) => {
            let class = cp_class_name(cp, *c);
            let (n, d) = cp_name_and_type(cp, *nt);
            format!("Method({class}.{n}:{d})")
        }
        Some(CpEntry::InterfaceMethodref(c, nt)) => {
            let class = cp_class_name(cp, *c);
            let (n, d) = cp_name_and_type(cp, *nt);
            format!("InterfaceMethod({class}.{n}:{d})")
        }
        Some(CpEntry::NameAndType(n, d)) => {
            format!("NameAndType({}:{})", cp_utf8(cp, *n), cp_utf8(cp, *d))
        }
        _ => format!("#{idx}"),
    }
}

fn cp_name_and_type(cp: &[CpEntry], idx: u16) -> (String, String) {
    match cp.get(idx as usize) {
        Some(CpEntry::NameAndType(n, d)) => {
            (cp_utf8(cp, *n).to_string(), cp_utf8(cp, *d).to_string())
        }
        _ => ("<?>".into(), "<?>".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny class file by hand: empty `main()` method, no
    /// constants beyond what's required.
    fn minimal_class() -> Vec<u8> {
        // Use the JVM backend's actual emitter via the test crate
        // graph would create a circular dep. Instead, we construct a
        // valid-but-empty class file by hand.
        //
        // We just verify that the parser walks it without erroring;
        // golden tests live in skotch-backend-jvm.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        bytes.extend_from_slice(&[0, 0]); // minor
        bytes.extend_from_slice(&[0, 61]); // major 61
        bytes.extend_from_slice(&[0, 4]); // cp_count = 4 (entries 1..3)
                                          // #1 Utf8 "X"
        bytes.push(1);
        bytes.extend_from_slice(&(1u16).to_be_bytes());
        bytes.push(b'X');
        // #2 Utf8 "java/lang/Object"
        bytes.push(1);
        let s = b"java/lang/Object";
        bytes.extend_from_slice(&(s.len() as u16).to_be_bytes());
        bytes.extend_from_slice(s);
        // #3 Class -> #2
        bytes.push(7);
        bytes.extend_from_slice(&(2u16).to_be_bytes());
        // We need a Class entry pointing at #1 too for this_class.
        // But we declared cp_count = 4 → only 3 entries. Re-do with
        // cp_count = 5 and add a fourth Class entry pointing at #1.
        bytes.clear();
        bytes.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&[0, 61]);
        bytes.extend_from_slice(&[0, 5]); // cp_count = 5
        bytes.push(1);
        bytes.extend_from_slice(&(1u16).to_be_bytes());
        bytes.push(b'X');
        bytes.push(1);
        let s = b"java/lang/Object";
        bytes.extend_from_slice(&(s.len() as u16).to_be_bytes());
        bytes.extend_from_slice(s);
        bytes.push(7);
        bytes.extend_from_slice(&(1u16).to_be_bytes()); // Class -> "X"
        bytes.push(7);
        bytes.extend_from_slice(&(2u16).to_be_bytes()); // Class -> "java/lang/Object"

        bytes.extend_from_slice(&[0x00, 0x21]); // access_flags = ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&(3u16).to_be_bytes()); // this_class -> #3
        bytes.extend_from_slice(&(4u16).to_be_bytes()); // super_class -> #4
        bytes.extend_from_slice(&[0, 0]); // interfaces_count
        bytes.extend_from_slice(&[0, 0]); // fields_count
        bytes.extend_from_slice(&[0, 0]); // methods_count
        bytes.extend_from_slice(&[0, 0]); // attributes_count
        bytes
    }

    #[test]
    fn parses_minimal_class() {
        let bytes = minimal_class();
        let n = normalize_default(&bytes).unwrap();
        let text = n.into_text();
        assert!(text.contains("class_version major=61"));
        assert!(text.contains("this_class    X"));
        assert!(text.contains("super_class   java/lang/Object"));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = minimal_class();
        bytes[0] = 0;
        assert!(normalize_default(&bytes).is_err());
    }

    #[test]
    fn cp_symbolic_renders_string() {
        let cp = vec![
            CpEntry::Reserved,
            CpEntry::Utf8("hello".into()),
            CpEntry::String(1),
        ];
        assert_eq!(cp_symbolic(&cp, 2), "\"hello\"");
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: normalize_drops_source_file_attribute
    // TODO: normalize_sorts_methods_alphabetically
    // TODO: normalize_replaces_cp_indices_with_symbolic_in_bytecode
    // TODO: normalize_strips_kotlin_metadata_annotation
    // TODO: normalize_keeps_stack_map_table_when_present
}
