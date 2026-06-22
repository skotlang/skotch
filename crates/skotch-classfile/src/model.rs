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
    /// Class-level `RuntimeVisible`/`RuntimeInvisibleAnnotations`, in declaration order.
    pub annotations: Vec<ClassAnnotation>,
    /// The class's generic `Signature` attribute, if any (e.g. `<T:Ljava/lang/Number;>L...;`).
    pub signature: Option<String>,
    /// `InnerClasses` attribute entries, in declaration order.
    pub inner_classes: Vec<InnerClassEntry>,
    /// `EnclosingMethod` attribute (present for local/anonymous classes).
    pub enclosing_method: Option<EnclosingMethod>,
}

/// One `InnerClasses` attribute entry.
#[derive(Debug, Clone)]
pub struct InnerClassEntry {
    /// Internal name of the inner class, e.g. `Outer$Inner`.
    pub inner: String,
    /// Internal name of the enclosing class, or `None` for local/anonymous classes.
    pub outer: Option<String>,
    /// The simple source name, or `None` for anonymous classes.
    pub inner_name: Option<String>,
    pub access_flags: u16,
}

/// The `EnclosingMethod` attribute: the class (and optionally method) that lexically encloses
/// a local or anonymous class.
#[derive(Debug, Clone)]
pub struct EnclosingMethod {
    /// Internal name of the enclosing class.
    pub class: String,
    /// Enclosing method name + descriptor, or `None` if the class is enclosed directly by a
    /// class (e.g. in a field initializer / instance initializer).
    pub method: Option<(String, String)>,
}

/// A class-level Java annotation.
#[derive(Debug, Clone)]
pub struct ClassAnnotation {
    /// DEX annotation visibility: 1 = RUNTIME (RuntimeVisible), 0 = BUILD (RuntimeInvisible).
    pub visibility: u8,
    /// Type descriptor, e.g. `LAnn;`.
    pub type_desc: String,
    /// Element-value pairs in declaration order (empty for a marker annotation). If any value
    /// is `Unsupported`, the dexer skips this annotation rather than emit a wrong value.
    pub elements: Vec<AnnotationElement>,
}

/// One `element_value_pair` of a Java annotation.
#[derive(Debug, Clone)]
pub struct AnnotationElement {
    pub name: String,
    pub value: AnnElemValue,
}

/// A Java annotation element value. Only the variants the dexer can encode are modeled;
/// anything else (enum/class/nested-annotation/byte/char/short/etc.) is `Unsupported`.
#[derive(Debug, Clone)]
pub enum AnnElemValue {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Boolean(bool),
    Str(String),
    Array(Vec<AnnElemValue>),
    /// An enum constant ('e' tag): the enum type descriptor + the constant's name.
    Enum {
        type_desc: String,
        const_name: String,
    },
    /// A value tag the dexer does not yet emit (class 'c', nested '@', byte/char/short).
    Unsupported,
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
    /// `RuntimeVisible`/`RuntimeInvisibleAnnotations` on this member.
    pub annotations: Vec<ClassAnnotation>,
    /// The member's generic `Signature` attribute, if any.
    pub signature: Option<String>,
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
