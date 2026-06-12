//! Parsed `.class` file model.

use crate::constant_pool::ConstantPool;

/// A parsed class file.
#[derive(Debug, Clone)]
pub struct ClassFile {
    pub minor_version: u16,
    pub major_version: u16,
    pub constant_pool: ConstantPool,
    pub access_flags: u16,
    /// Internal name, e.g. `Empty` or `java/lang/Object`.
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
    pub source_file: Option<String>,
    pub bootstrap_methods: Vec<BootstrapMethod>,
}

impl ClassFile {
    /// DEX type descriptor of this class, e.g. `LEmpty;`.
    pub fn descriptor(&self) -> String {
        crate::constant_pool::internal_to_descriptor(&self.this_class)
    }
}

/// A field or method.
#[derive(Debug, Clone)]
pub struct Member {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    pub code: Option<Code>,
    /// `ConstantValue` index for static fields (if any).
    pub constant_value: Option<crate::constant_pool::Constant>,
}

impl Member {
    pub fn is_static(&self) -> bool {
        self.access_flags & 0x0008 != 0
    }
    pub fn is_abstract(&self) -> bool {
        self.access_flags & 0x0400 != 0
    }
    pub fn is_native(&self) -> bool {
        self.access_flags & 0x0100 != 0
    }
}

/// A parsed `Code` attribute.
#[derive(Debug, Clone)]
pub struct Code {
    pub max_stack: u16,
    pub max_locals: u16,
    pub bytecode: Vec<u8>,
    pub exceptions: Vec<ExceptionEntry>,
    pub line_numbers: Vec<(u16, u16)>, // (start_pc, line)
    pub local_variables: Vec<LocalVariable>,
}

#[derive(Debug, Clone)]
pub struct ExceptionEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    /// Caught type internal name, or `None` for `finally` (catch-all).
    pub catch_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocalVariable {
    pub start_pc: u16,
    pub length: u16,
    pub name: String,
    pub descriptor: String,
    pub index: u16,
}

#[derive(Debug, Clone)]
pub struct BootstrapMethod {
    pub method_handle_index: u16,
    pub arguments: Vec<u16>,
}
