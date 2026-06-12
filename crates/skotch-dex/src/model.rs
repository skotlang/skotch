//! A symbolic DEX program model. References (strings, types, fields, methods)
//! are held by value; the [`crate::writer`] interns and sorts them into pools,
//! assigns indices the way d8 does, and patches instruction operands.

/// A field reference: defining class + type + name (all type descriptors /
/// names, e.g. class `"LFoo;"`, type `"I"`, name `"count"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FieldRef {
    pub class: String,
    pub type_: String,
    pub name: String,
}

/// A method prototype: return type + parameter types (descriptors).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProtoRef {
    pub return_type: String,
    pub params: Vec<String>,
}

impl ProtoRef {
    /// The "shorty" descriptor: one char per return + each param
    /// (`V/Z/B/S/C/I/J/F/D` for primitives, `L` for any reference/array).
    pub fn shorty(&self) -> String {
        let mut s = String::with_capacity(1 + self.params.len());
        s.push(shorty_char(&self.return_type));
        for p in &self.params {
            s.push(shorty_char(p));
        }
        s
    }
}

fn shorty_char(descriptor: &str) -> char {
    match descriptor.as_bytes().first() {
        Some(b'V') => 'V',
        Some(b'Z') => 'Z',
        Some(b'B') => 'B',
        Some(b'S') => 'S',
        Some(b'C') => 'C',
        Some(b'I') => 'I',
        Some(b'J') => 'J',
        Some(b'F') => 'F',
        Some(b'D') => 'D',
        _ => 'L', // L... or [...
    }
}

/// A method reference: defining class + prototype + name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MethodRef {
    pub class: String,
    pub proto: ProtoRef,
    pub name: String,
}

/// An operand in an instruction that refers to a pool item and must be patched
/// with the item's final index after the pools are built.
#[derive(Debug, Clone)]
pub enum ItemRef {
    String(String),
    Type(String),
    Field(FieldRef),
    Method(MethodRef),
    Proto(ProtoRef),
    CallSite(usize),
}

/// A patch site inside a code item's instruction stream: write the resolved
/// index of `item` into the 16-bit code unit at `unit` (and, for index kinds
/// wider than 16 bits such as `const-string/jumbo`, the following unit too).
#[derive(Debug, Clone)]
pub struct Fixup {
    pub unit: usize,
    pub item: ItemRef,
    /// True for 32-bit index operands (jumbo string).
    pub wide: bool,
}

/// A try/catch region in a code item.
#[derive(Debug, Clone)]
pub struct TryItem {
    pub start_addr: u32,
    pub insn_count: u16,
    pub handlers: Vec<CatchHandler>,
    pub catch_all_addr: Option<u32>,
}

/// One typed catch handler.
#[derive(Debug, Clone)]
pub struct CatchHandler {
    pub exception_type: String,
    pub addr: u32,
}

/// Structured debug info (matches `debug_info_item`).
#[derive(Debug, Clone, Default)]
pub struct DebugInfo {
    pub line_start: u32,
    /// Parameter names (`None` for unnamed). Encoded as `uleb128p1` string idx.
    pub parameter_names: Vec<Option<String>>,
    pub events: Vec<DebugEvent>,
}

/// One debug state-machine event.
#[derive(Debug, Clone)]
pub enum DebugEvent {
    AdvancePc { addr_diff: u32 },
    AdvanceLine { line_diff: i32 },
    StartLocal { register: u32, name: Option<String>, type_: Option<String> },
    StartLocalExtended { register: u32, name: Option<String>, type_: Option<String>, sig: Option<String> },
    EndLocal { register: u32 },
    RestartLocal { register: u32 },
    SetPrologueEnd,
    SetEpilogueBegin,
    SetFile { name: Option<String> },
    /// Special opcode (0x0a..=0xff): combined line+addr advance.
    Special(u8),
}

/// A method's code item.
#[derive(Debug, Clone)]
pub struct CodeItem {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    /// Instruction stream as 16-bit code units (operands already encoded;
    /// pool indices are placeholders patched via `fixups`).
    pub insns: Vec<u16>,
    pub fixups: Vec<Fixup>,
    pub tries: Vec<TryItem>,
    pub debug_info: Option<DebugInfo>,
}

/// A field with its access flags.
#[derive(Debug, Clone)]
pub struct EncodedField {
    pub field: FieldRef,
    pub access_flags: u32,
}

/// A method with its access flags and optional code.
#[derive(Debug, Clone)]
pub struct EncodedMethod {
    pub method: MethodRef,
    pub access_flags: u32,
    pub code: Option<CodeItem>,
}

/// A class definition.
#[derive(Debug, Clone)]
pub struct ClassDef {
    pub class_type: String,
    pub access_flags: u32,
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    pub source_file: Option<String>,
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
    /// `static_field` initial values (`encoded_array`), if any.
    pub static_values: Vec<EncodedValue>,
}

/// A subset of `encoded_value` sufficient for static field initializers.
#[derive(Debug, Clone)]
pub enum EncodedValue {
    Int(i32),
    Long(i64),
    Boolean(bool),
    String(String),
    Null,
}

/// A whole DEX file model.
#[derive(Debug, Clone, Default)]
pub struct DexFile {
    pub classes: Vec<ClassDef>,
    /// Extra strings to include in the pool even if unreferenced — used for
    /// d8's `~~D8{…}` synthetic marker.
    pub extra_strings: Vec<String>,
}
