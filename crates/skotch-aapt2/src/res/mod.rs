//! Core resource identity model: resource types, names, and IDs.
//!
//! Port of aapt2's `Resource.h`/`Resource.cpp`. A resource is uniquely
//! identified by `package:type/entry` and (once assigned) a 32-bit ID
//! of the form `0xPPTTEEEE`.

pub mod config;
pub mod string_pool;
pub mod table;
pub mod utils;
pub mod value;

use std::fmt;

/// The set of logical resource types understood by the resource system.
///
/// Mirrors `aapt::ResourceType`. The enum order matters: types without
/// assigned IDs sort by this declaration order when a table is flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceType {
    Anim,
    Animator,
    Array,
    Attr,
    AttrPrivate,
    Bool,
    Color,
    /// Not really a type, but it shows up in some CTS tests and must keep
    /// being respected (`configVarying`).
    ConfigVarying,
    Dimen,
    Drawable,
    Font,
    Fraction,
    Id,
    Integer,
    Interpolator,
    Layout,
    Macro,
    Menu,
    Mipmap,
    Navigation,
    Plurals,
    Raw,
    String,
    Style,
    Styleable,
    Transition,
    Xml,
}

impl ResourceType {
    /// The canonical name as it appears in `package:type/entry` syntax
    /// and in `res/<type>-<config>/` directory names.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceType::Anim => "anim",
            ResourceType::Animator => "animator",
            ResourceType::Array => "array",
            ResourceType::Attr => "attr",
            ResourceType::AttrPrivate => "^attr-private",
            ResourceType::Bool => "bool",
            ResourceType::Color => "color",
            ResourceType::ConfigVarying => "configVarying",
            ResourceType::Dimen => "dimen",
            ResourceType::Drawable => "drawable",
            ResourceType::Font => "font",
            ResourceType::Fraction => "fraction",
            ResourceType::Id => "id",
            ResourceType::Integer => "integer",
            ResourceType::Interpolator => "interpolator",
            ResourceType::Layout => "layout",
            ResourceType::Macro => "macro",
            ResourceType::Menu => "menu",
            ResourceType::Mipmap => "mipmap",
            ResourceType::Navigation => "navigation",
            ResourceType::Plurals => "plurals",
            ResourceType::Raw => "raw",
            ResourceType::String => "string",
            ResourceType::Style => "style",
            ResourceType::Styleable => "styleable",
            ResourceType::Transition => "transition",
            ResourceType::Xml => "xml",
        }
    }

    /// Parses a type name (`"drawable"`, `"^attr-private"`, …).
    pub fn parse(s: &str) -> Option<ResourceType> {
        Some(match s {
            "anim" => ResourceType::Anim,
            "animator" => ResourceType::Animator,
            "array" => ResourceType::Array,
            "attr" => ResourceType::Attr,
            "^attr-private" => ResourceType::AttrPrivate,
            "bool" => ResourceType::Bool,
            "color" => ResourceType::Color,
            "configVarying" => ResourceType::ConfigVarying,
            "dimen" => ResourceType::Dimen,
            "drawable" => ResourceType::Drawable,
            "font" => ResourceType::Font,
            "fraction" => ResourceType::Fraction,
            "id" => ResourceType::Id,
            "integer" => ResourceType::Integer,
            "interpolator" => ResourceType::Interpolator,
            "layout" => ResourceType::Layout,
            "macro" => ResourceType::Macro,
            "menu" => ResourceType::Menu,
            "mipmap" => ResourceType::Mipmap,
            "navigation" => ResourceType::Navigation,
            "plurals" => ResourceType::Plurals,
            "raw" => ResourceType::Raw,
            "string" => ResourceType::String,
            "style" => ResourceType::Style,
            "styleable" => ResourceType::Styleable,
            "transition" => ResourceType::Transition,
            "xml" => ResourceType::Xml,
            _ => return None,
        })
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A type as stored in a resource table: the logical [`ResourceType`]
/// plus the actual (possibly custom-suffixed) name, e.g. `string.v2`.
///
/// Mirrors `aapt::ResourceNamedType`. Ordering is by logical type first,
/// then by name — this drives type ordering during flattening.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceNamedType {
    pub name: String,
    pub ty: ResourceType,
}

impl ResourceNamedType {
    pub fn with_default_name(ty: ResourceType) -> Self {
        ResourceNamedType {
            name: ty.as_str().to_string(),
            ty,
        }
    }

    /// Parses `"string"` or a custom-named type like `"string.foo"` where
    /// the prefix before the first `.` must be a valid resource type.
    pub fn parse(s: &str) -> Option<ResourceNamedType> {
        let ty = match s.find('.') {
            // A trailing dot is not a custom name separator.
            Some(dot) if dot + 1 < s.len() => ResourceType::parse(&s[..dot])?,
            _ => ResourceType::parse(s)?,
        };
        Some(ResourceNamedType {
            name: s.to_string(),
            ty,
        })
    }
}

impl PartialOrd for ResourceNamedType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ResourceNamedType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ty
            .cmp(&other.ty)
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl fmt::Display for ResourceNamedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// A resource's full name: `package:type/entry`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceName {
    pub package: String,
    pub ty: ResourceNamedType,
    pub entry: String,
}

impl ResourceName {
    pub fn new(package: impl Into<String>, ty: ResourceType, entry: impl Into<String>) -> Self {
        ResourceName {
            package: package.into(),
            ty: ResourceNamedType::with_default_name(ty),
            entry: entry.into(),
        }
    }

    pub fn with_named_type(
        package: impl Into<String>,
        ty: ResourceNamedType,
        entry: impl Into<String>,
    ) -> Self {
        ResourceName {
            package: package.into(),
            ty,
            entry: entry.into(),
        }
    }

    pub fn is_valid(&self) -> bool {
        !self.package.is_empty() && !self.entry.is_empty()
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.package.is_empty() {
            write!(f, "{}:", self.package)?;
        }
        write!(f, "{}/{}", self.ty, self.entry)
    }
}

/// The package ID reserved for the running application.
pub const APP_PACKAGE_ID: u8 = 0x7f;
/// The package ID reserved for the `android` framework package.
pub const FRAMEWORK_PACKAGE_ID: u8 = 0x01;

/// A binary resource identifier: `0xPPTTEEEE`.
///
/// `PP` is the package ID (0x01 system, 0x7f app), `TT` the type ID
/// (0x00 invalid), `EEEE` the entry ID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(pub u32);

impl ResourceId {
    pub fn new(package: u8, ty: u8, entry: u16) -> Self {
        ResourceId(((package as u32) << 24) | ((ty as u32) << 16) | entry as u32)
    }

    /// Valid and not dynamic: both package ID and type ID are non-zero.
    pub fn is_valid_static(self) -> bool {
        (self.0 & 0xff00_0000) != 0 && (self.0 & 0x00ff_0000) != 0
    }

    /// Valid, allowing a dynamic (zero) package ID.
    pub fn is_valid(self) -> bool {
        (self.0 & 0x00ff_0000) != 0
    }

    pub fn package_id(self) -> u8 {
        (self.0 >> 24) as u8
    }

    pub fn type_id(self) -> u8 {
        (self.0 >> 16) as u8
    }

    pub fn entry_id(self) -> u16 {
        self.0 as u16
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:08x}", self.0)
    }
}

/// Whether a value sits behind a feature flag, and whether that flag was
/// enabled when the value was compiled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlagStatus {
    #[default]
    NoFlag = 0,
    Disabled = 1,
    Enabled = 2,
}

impl FlagStatus {
    pub fn from_u32(v: u32) -> FlagStatus {
        match v {
            1 => FlagStatus::Disabled,
            2 => FlagStatus::Enabled,
            _ => FlagStatus::NoFlag,
        }
    }
}

/// A `android:featureFlag="[!]flag.name"` attribute occurrence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlagAttribute {
    pub name: String,
    pub negated: bool,
}

impl fmt::Display for FeatureFlagAttribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            write!(f, "!")?;
        }
        f.write_str(&self.name)
    }
}

/// Source file (and optional line) a resource value came from.
/// Mirrors `android::Source` — used purely for diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Source {
    pub path: String,
    pub line: Option<usize>,
    /// Original archive (e.g. the `.flat` file) the source was packaged in.
    pub archive: Option<String>,
}

impl Source {
    pub fn new(path: impl Into<String>) -> Self {
        Source {
            path: path.into(),
            line: None,
            archive: None,
        }
    }

    pub fn with_line(path: impl Into<String>, line: usize) -> Self {
        Source {
            path: path.into(),
            line: Some(line),
            archive: None,
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(archive) = &self.archive {
            write!(f, "{}@{}", archive, self.path)?;
        } else {
            f.write_str(&self.path)?;
        }
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        Ok(())
    }
}

/// Metadata describing a compiled resource file (layout XML, PNG, …).
/// Mirrors `aapt::ResourceFile`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResourceFile {
    pub name: ResourceName,
    pub config: config::ConfigDescription,
    pub file_type: value::FileType,
    pub source: Source,
    /// Symbols exported by the file (`@+id/foo`), with the line each was
    /// defined on.
    pub exported_symbols: Vec<SourcedResourceName>,
    pub flag_status: FlagStatus,
    pub flag: Option<FeatureFlagAttribute>,
}

/// A resource name plus the line it appeared on (for exported symbols).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourcedResourceName {
    pub name: ResourceName,
    pub line: usize,
}

impl Default for ResourceName {
    fn default() -> Self {
        ResourceName {
            package: String::new(),
            ty: ResourceNamedType::with_default_name(ResourceType::Raw),
            entry: String::new(),
        }
    }
}

// The faithful `ResourceUtils::ParseResourceName` port lives in
// [`utils`]; re-exported here since it's used throughout the crate.
pub use utils::parse_resource_name;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_round_trip() {
        for ty in [
            ResourceType::Anim,
            ResourceType::AttrPrivate,
            ResourceType::ConfigVarying,
            ResourceType::Xml,
        ] {
            assert_eq!(ResourceType::parse(ty.as_str()), Some(ty));
        }
        assert_eq!(ResourceType::parse("not_a_type"), None);
    }

    #[test]
    fn named_type_custom_suffix() {
        let t = ResourceNamedType::parse("string.foo").unwrap();
        assert_eq!(t.ty, ResourceType::String);
        assert_eq!(t.name, "string.foo");
        assert!(ResourceNamedType::parse("nope.foo").is_none());
    }

    #[test]
    fn resource_id_parts() {
        let id = ResourceId::new(0x7f, 0x02, 0x0001);
        assert_eq!(id.0, 0x7f02_0001);
        assert_eq!(id.package_id(), 0x7f);
        assert_eq!(id.type_id(), 0x02);
        assert_eq!(id.entry_id(), 1);
        assert!(id.is_valid_static());
        assert!(ResourceId(0x0002_0001).is_valid());
        assert!(!ResourceId(0x0002_0001).is_valid_static());
        assert_eq!(id.to_string(), "0x7f020001");
    }

    #[test]
    fn parse_full_names() {
        let (name, private) = parse_resource_name("android:string/ok").unwrap();
        assert_eq!(name.package, "android");
        assert_eq!(name.ty.ty, ResourceType::String);
        assert_eq!(name.entry, "ok");
        assert!(!private);

        let (name, private) = parse_resource_name("*android:string/ok").unwrap();
        assert!(private);
        assert_eq!(name.package, "android");

        let (name, _) = parse_resource_name("drawable/icon").unwrap();
        assert_eq!(name.package, "");
        assert_eq!(name.to_string(), "drawable/icon");

        assert!(parse_resource_name("drawable").is_none());
        assert!(parse_resource_name("").is_none());
    }
}
