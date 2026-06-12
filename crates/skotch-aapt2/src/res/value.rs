//! Resource value model.
//!
//! Port of aapt2's `ResourceValues.h`. The C++ class hierarchy
//! (`Value` → `Item` → concrete types) maps to two Rust enums:
//! [`Item`] for values that can appear inline (XML attributes, style
//! entries, array elements) and [`Value`] for everything a table entry
//! can hold (an item or a compound value).
//!
//! Also hosts the `android::Res_value` binary representation
//! ([`ResValue`]) and its data-type constants, ported from
//! `androidfw/ResourceTypes.h`.

use super::{FeatureFlagAttribute, FlagStatus, ResourceId, ResourceName, Source};
use std::fmt;

/// Binary representation of a single resource value
/// (`android::Res_value`). 8 bytes on disk: size, res0, dataType, data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ResValue {
    pub data_type: u8,
    pub data: u32,
}

impl ResValue {
    pub const SIZE: u16 = 8;

    pub fn new(data_type: u8, data: u32) -> Self {
        ResValue { data_type, data }
    }
}

/// `Res_value` data types.
pub mod res_value_type {
    pub const TYPE_NULL: u8 = 0x00;
    pub const TYPE_REFERENCE: u8 = 0x01;
    pub const TYPE_ATTRIBUTE: u8 = 0x02;
    pub const TYPE_STRING: u8 = 0x03;
    pub const TYPE_FLOAT: u8 = 0x04;
    pub const TYPE_DIMENSION: u8 = 0x05;
    pub const TYPE_FRACTION: u8 = 0x06;
    pub const TYPE_DYNAMIC_REFERENCE: u8 = 0x07;
    pub const TYPE_DYNAMIC_ATTRIBUTE: u8 = 0x08;
    pub const TYPE_FIRST_INT: u8 = 0x10;
    pub const TYPE_INT_DEC: u8 = 0x10;
    pub const TYPE_INT_HEX: u8 = 0x11;
    pub const TYPE_INT_BOOLEAN: u8 = 0x12;
    pub const TYPE_FIRST_COLOR_INT: u8 = 0x1c;
    pub const TYPE_INT_COLOR_ARGB8: u8 = 0x1c;
    pub const TYPE_INT_COLOR_RGB8: u8 = 0x1d;
    pub const TYPE_INT_COLOR_ARGB4: u8 = 0x1e;
    pub const TYPE_INT_COLOR_RGB4: u8 = 0x1f;
    pub const TYPE_LAST_COLOR_INT: u8 = 0x1f;
    pub const TYPE_LAST_INT: u8 = 0x1f;
}

/// `Res_value` complex-number encoding (dimensions and fractions).
pub mod complex {
    pub const UNIT_SHIFT: u32 = 0;
    pub const UNIT_MASK: u32 = 0xf;
    pub const UNIT_PX: u32 = 0;
    pub const UNIT_DIP: u32 = 1;
    pub const UNIT_SP: u32 = 2;
    pub const UNIT_PT: u32 = 3;
    pub const UNIT_IN: u32 = 4;
    pub const UNIT_MM: u32 = 5;
    pub const UNIT_FRACTION: u32 = 0;
    pub const UNIT_FRACTION_PARENT: u32 = 1;
    pub const RADIX_SHIFT: u32 = 4;
    pub const RADIX_MASK: u32 = 0x3;
    pub const RADIX_23P0: u32 = 0;
    pub const RADIX_16P7: u32 = 1;
    pub const RADIX_8P15: u32 = 2;
    pub const RADIX_0P23: u32 = 3;
    pub const MANTISSA_SHIFT: u32 = 8;
    pub const MANTISSA_MASK: u32 = 0xffffff;
}

/// `Res_value` data for `TYPE_NULL`.
pub const DATA_NULL_UNDEFINED: u32 = 0;
pub const DATA_NULL_EMPTY: u32 = 1;

/// Metadata common to every value: where it was defined, its comment,
/// weakness, translatability, and feature-flag state.
///
/// Mirrors the protected members of C++ `aapt::Value`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueMeta {
    pub source: Source,
    pub comment: String,
    /// Weak values can be overridden without warning or error.
    pub weak: bool,
    /// Only used during compilation; not persisted to binary.
    pub translatable: bool,
    pub flag: Option<FeatureFlagAttribute>,
    pub flag_status: FlagStatus,
}

impl ValueMeta {
    pub fn new() -> Self {
        ValueMeta { translatable: true, ..Default::default() }
    }
}

/// Reference type: plain resource reference (`@…`) or theme attribute
/// reference (`?…`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ReferenceType {
    #[default]
    Resource,
    Attribute,
}

/// A reference to another resource, symbolic (by name) and/or numeric
/// (by ID). Maps to `TYPE_REFERENCE` / `TYPE_ATTRIBUTE`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reference {
    pub name: Option<ResourceName>,
    pub id: Option<ResourceId>,
    /// Type flags used when the reference was compiled (macro support).
    pub type_flags: Option<u32>,
    pub reference_type: ReferenceType,
    pub private_reference: bool,
    pub is_dynamic: bool,
    /// Whether raw strings were acceptable in this position (macro support).
    pub allow_raw: bool,
}

impl Reference {
    pub fn from_name(name: ResourceName) -> Self {
        Reference { name: Some(name), ..Default::default() }
    }

    pub fn from_id(id: ResourceId) -> Self {
        Reference { id: Some(id), ..Default::default() }
    }
}

/// A span of styling applied to a [`StyledString`], with UTF-16
/// character offsets (inclusive on both ends, matching aapt2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    /// Tag name plus encoded attributes: `tag;attr1=value1;attr2=value2;…`
    pub name: String,
    pub first_char: u32,
    pub last_char: u32,
}

/// A byte range of a string that must not be translated/pseudolocalized.
/// Start inclusive, end exclusive. Compile-time only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UntranslatableSection {
    pub start: usize,
    pub end: usize,
}

/// An item: a value that can appear inline (attribute value, style
/// entry, array element). Mirrors the C++ `Item` subclasses.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `TYPE_REFERENCE`/`TYPE_ATTRIBUTE` reference to another resource.
    Reference(Reference),
    /// Placeholder occupying a resource ID; the value is unimportant.
    Id,
    /// Unprocessed string (quotes, escapes and whitespace intact). Never
    /// appears in a final table.
    RawString(String),
    /// A plain string.
    String {
        value: String,
        /// Compile-time-only pseudolocalization exclusions.
        untranslatable_sections: Vec<UntranslatableSection>,
    },
    /// A string carrying HTML-like styling spans.
    StyledString {
        value: String,
        spans: Vec<Span>,
        untranslatable_sections: Vec<UntranslatableSection>,
    },
    /// A reference to a file within the APK.
    FileReference(FileReference),
    /// Any other `Res_value` (ints, floats, colors, dimensions, …).
    BinaryPrimitive(ResValue),
}

/// File type of a [`FileReference`] target. Mirrors `ResourceFile::Type`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FileType {
    #[default]
    Unknown,
    Png,
    BinaryXml,
    ProtoXml,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileReference {
    /// Path to the file within the APK (`res/type-config/entry.ext`).
    pub path: String,
    /// How to interpret the file contents.
    pub file_type: FileType,
    /// Compile-phase handle to the file contents; not persisted.
    pub file_contents: Option<std::sync::Arc<Vec<u8>>>,
}

/// An attribute definition (`<attr>`): allowed formats and enum/flag
/// symbols.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub type_mask: u32,
    pub min_int: i32,
    pub max_int: i32,
    pub symbols: Vec<AttributeSymbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttributeSymbol {
    pub symbol: Reference,
    /// Source/comment for the enum/flag item itself.
    pub source: Source,
    pub comment: String,
    pub value: u32,
    /// The `Res_value` data type of the symbol value.
    pub data_type: u8,
}

/// Attribute format mask bits (match `android::ResTable_map` and
/// `aapt.pb.Attribute.FormatFlags`).
pub mod format {
    pub const ANY: u32 = 0x0000_ffff;
    pub const REFERENCE: u32 = 0x01;
    pub const STRING: u32 = 0x02;
    pub const INTEGER: u32 = 0x04;
    pub const BOOLEAN: u32 = 0x08;
    pub const COLOR: u32 = 0x10;
    pub const FLOAT: u32 = 0x20;
    pub const DIMENSION: u32 = 0x40;
    pub const FRACTION: u32 = 0x80;
    pub const ENUM: u32 = 0x0001_0000;
    pub const FLAGS: u32 = 0x0002_0000;
}

impl Attribute {
    pub fn new(type_mask: u32) -> Self {
        Attribute {
            type_mask,
            min_int: i32::MIN,
            max_int: i32::MAX,
            symbols: Vec::new(),
        }
    }

    /// Human-readable rendering of the format mask, e.g.
    /// `reference|string`. Mirrors `Attribute::MaskString`.
    pub fn mask_string(type_mask: u32) -> String {
        use format::*;
        if type_mask == ANY {
            return "any".to_string();
        }
        let mut out = String::new();
        let mut push = |s: &str| {
            if !out.is_empty() {
                out.push('|');
            }
            out.push_str(s);
        };
        if type_mask & REFERENCE != 0 {
            push("reference");
        }
        if type_mask & STRING != 0 {
            push("string");
        }
        if type_mask & INTEGER != 0 {
            push("integer");
        }
        if type_mask & BOOLEAN != 0 {
            push("boolean");
        }
        if type_mask & COLOR != 0 {
            push("color");
        }
        if type_mask & FLOAT != 0 {
            push("float");
        }
        if type_mask & DIMENSION != 0 {
            push("dimension");
        }
        if type_mask & FRACTION != 0 {
            push("fraction");
        }
        if type_mask & ENUM != 0 {
            push("enum");
        }
        if type_mask & FLAGS != 0 {
            push("flags");
        }
        out
    }
}

/// A style (`<style>`): optional parent plus attribute/value entries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub parent: Option<Reference>,
    /// True when the parent was inferred from a dotted style name.
    pub parent_inferred: bool,
    /// Source of the parent declaration (diagnostics only).
    pub parent_source: Source,
    pub entries: Vec<StyleEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyleEntry {
    pub key: Reference,
    pub value: ItemValue,
    pub source: Source,
    pub comment: String,
}

/// A `<declare-styleable>`: a set of attribute references. Only lands in
/// R.java, never in the binary table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Styleable {
    pub entries: Vec<Reference>,
}

/// An `<array>` (including string-array/integer-array).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Array {
    pub elements: Vec<ItemValue>,
}

/// Plural arity slots, ordered as in `aapt::Plural`.
pub const PLURAL_ZERO: usize = 0;
pub const PLURAL_ONE: usize = 1;
pub const PLURAL_TWO: usize = 2;
pub const PLURAL_FEW: usize = 3;
pub const PLURAL_MANY: usize = 4;
pub const PLURAL_OTHER: usize = 5;
pub const PLURAL_COUNT: usize = 6;

/// A `<plurals>` resource: one optional item per arity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plural {
    pub values: [Option<ItemValue>; PLURAL_COUNT],
}

pub fn plural_arity_name(index: usize) -> &'static str {
    match index {
        PLURAL_ZERO => "zero",
        PLURAL_ONE => "one",
        PLURAL_TWO => "two",
        PLURAL_FEW => "few",
        PLURAL_MANY => "many",
        _ => "other",
    }
}

/// A `<macro>` definition (compile-time substitution body).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Macro {
    pub raw_value: String,
    pub style_string: StyleString,
    pub untranslatable_sections: Vec<UntranslatableSection>,
    pub alias_namespaces: Vec<MacroNamespace>,
}

/// An unresolved styled string (pre-StringPool form), used by macros.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleString {
    pub str: String,
    pub spans: Vec<StyleStringSpan>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StyleStringSpan {
    pub name: String,
    pub first_char: u32,
    pub last_char: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacroNamespace {
    pub alias: String,
    pub package_name: String,
    pub is_private: bool,
}

/// An [`Item`] with its value metadata. This is what style entries,
/// array elements, and plural slots hold.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemValue {
    pub item: Item,
    pub meta: ValueMeta,
}

impl ItemValue {
    pub fn new(item: Item) -> Self {
        ItemValue { item, meta: ValueMeta::new() }
    }
}

/// Any value a resource table entry can hold.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueKind {
    Item(Item),
    Attribute(Attribute),
    Style(Style),
    Styleable(Styleable),
    Array(Array),
    Plural(Plural),
    Macro(Macro),
}

/// A complete value: kind plus metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Value {
    pub kind: ValueKind,
    pub meta: ValueMeta,
}

impl Value {
    pub fn new(kind: ValueKind) -> Self {
        Value { kind, meta: ValueMeta::new() }
    }

    pub fn item(item: Item) -> Self {
        Value::new(ValueKind::Item(item))
    }

    pub fn as_item(&self) -> Option<&Item> {
        match &self.kind {
            ValueKind::Item(item) => Some(item),
            _ => None,
        }
    }

    /// Whether two values are duplicates for deduping/merging purposes.
    /// Mirrors C++ `Value::Equals` (metadata is not compared).
    pub fn equals(&self, other: &Value) -> bool {
        self.kind == other.kind
    }
}

impl Item {
    /// Flattens this item to its binary [`ResValue`] form. String-typed
    /// items need a string-pool index, so they can't be flattened here;
    /// the table flattener handles them. Mirrors `Item::Flatten` for the
    /// non-pool cases.
    pub fn flatten(&self) -> Option<ResValue> {
        use res_value_type::*;
        match self {
            Item::Reference(reference) => {
                let data_type = match (reference.reference_type, reference.is_dynamic) {
                    (ReferenceType::Resource, false) => TYPE_REFERENCE,
                    (ReferenceType::Resource, true) => TYPE_DYNAMIC_REFERENCE,
                    (ReferenceType::Attribute, false) => TYPE_ATTRIBUTE,
                    (ReferenceType::Attribute, true) => TYPE_DYNAMIC_ATTRIBUTE,
                };
                Some(ResValue::new(data_type, reference.id.map(|id| id.0).unwrap_or(0)))
            }
            Item::Id => Some(ResValue::new(TYPE_INT_BOOLEAN, 0)),
            Item::BinaryPrimitive(value) => Some(*value),
            Item::RawString(_)
            | Item::String { .. }
            | Item::StyledString { .. }
            | Item::FileReference(_) => None,
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.reference_type {
            ReferenceType::Resource => "@",
            ReferenceType::Attribute => "?",
        };
        if let Some(name) = &self.name {
            write!(f, "({prefix}{name})")?;
        } else if let Some(id) = self.id {
            write!(f, "({prefix}{id})")?;
        } else {
            write!(f, "({prefix}null)")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_string_matches_aapt2() {
        assert_eq!(Attribute::mask_string(format::ANY), "any");
        assert_eq!(
            Attribute::mask_string(format::REFERENCE | format::STRING),
            "reference|string"
        );
        assert_eq!(Attribute::mask_string(format::ENUM), "enum");
    }

    #[test]
    fn reference_flatten() {
        let mut r = Reference::from_id(ResourceId(0x7f010001));
        let v = Item::Reference(r.clone()).flatten().unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_REFERENCE);
        assert_eq!(v.data, 0x7f010001);

        r.reference_type = ReferenceType::Attribute;
        let v = Item::Reference(r).flatten().unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_ATTRIBUTE);
    }
}
