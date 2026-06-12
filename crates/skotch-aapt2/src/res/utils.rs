//! Resource value parsing.
//!
//! Port of `ResourceUtils.cpp` plus the androidfw primitives it leans
//! on (`ResTable::stringToInt`, `ResTable::stringToFloat`,
//! `ExtractResourceName`): turning the textual forms found in res XML
//! (`@string/foo`, `?attr/bar`, `#ff0000`, `12.5dp`, `true`, `0x10`)
//! into typed [`Item`]s.

use super::value::{
    complex, format, res_value_type, Attribute, Item, Reference, ReferenceType, ResValue,
    DATA_NULL_EMPTY,
};
use super::{ResourceId, ResourceName, ResourceNamedType, ResourceType};
use crate::util::trim_whitespace;

/// The API level resources compiled against development SDKs report.
pub const DEVELOPMENT_SDK_LEVEL: i32 = 10_000;

const DEVELOPMENT_SDK_CODE_NAMES: &[&str] = &[
    "Q",
    "R",
    "S",
    "Sv2",
    "Tiramisu",
    "UpsideDownCake",
    "VanillaIceCream",
    "Baklava",
];
const PRIVACY_SANDBOX_SUFFIX: &str = "PrivacySandbox";

/// Splits `[package:][type/]entry` into its parts, tolerating either
/// separator order. Port of `android::ExtractResourceName`. Returns
/// `None` when a separator is present but its part is empty.
pub fn extract_resource_name(s: &str) -> Option<(&str, &str, &str)> {
    let s = s.strip_prefix('@').unwrap_or(s);
    let mut package = "";
    let mut ty = "";
    let mut has_package_separator = false;
    let mut has_type_separator = false;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        if ty.is_empty() && c == '/' {
            has_type_separator = true;
            ty = &s[start..i];
            start = i + 1;
        } else if package.is_empty() && c == ':' {
            has_package_separator = true;
            package = &s[start..i];
            start = i + 1;
        }
    }
    let entry = &s[start..];
    if (has_package_separator && package.is_empty())
        || (has_type_separator && ty.is_empty())
    {
        return None;
    }
    Some((package, ty, entry))
}

/// Parses `[*][package:]type/entry`. Port of
/// `ResourceUtils::ParseResourceName`. Returns the name and whether it
/// was a private (`*`) reference.
pub fn parse_resource_name(s: &str) -> Option<(ResourceName, bool)> {
    if s.is_empty() {
        return None;
    }
    let (private, rest) = match s.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (package, ty, entry) = extract_resource_name(rest)?;
    let named_type = ResourceNamedType::parse(ty)?;
    if entry.is_empty() {
        return None;
    }
    Some((
        ResourceName::with_named_type(package, named_type, entry),
        private,
    ))
}

/// Result of parsing a `@…` reference.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedReference {
    pub name: ResourceName,
    /// `@+id/foo` — the reference creates the ID resource.
    pub create: bool,
    pub private_reference: bool,
}

/// Parses `@[+][*][package:]type/entry`. Port of
/// `ResourceUtils::ParseReference`.
pub fn parse_reference(s: &str) -> Option<ParsedReference> {
    let trimmed = trim_whitespace(s);
    let rest = trimmed.strip_prefix('@')?;
    let (create, rest) = match rest.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let (name, private) = parse_resource_name(rest)?;
    if create && private {
        return None;
    }
    if create && name.ty.ty != ResourceType::Id {
        return None;
    }
    Some(ParsedReference { name, create, private_reference: private })
}

/// Parses `?[package:][type/]entry` (type must be `attr` if present).
/// Port of `ResourceUtils::ParseAttributeReference`.
pub fn parse_attribute_reference(s: &str) -> Option<ResourceName> {
    let trimmed = trim_whitespace(s);
    let rest = trimmed.strip_prefix('?')?;
    let (package, ty, entry) = extract_resource_name(rest)?;
    if !ty.is_empty() && ty != "attr" {
        return None;
    }
    if entry.is_empty() {
        return None;
    }
    Some(ResourceName::new(package, ResourceType::Attr, entry))
}

/// Parses either reference form into a [`Reference`] item, also
/// reporting whether `@+id` creation was requested. Port of
/// `ResourceUtils::TryParseReference`.
pub fn try_parse_reference(s: &str) -> Option<(Reference, bool)> {
    if let Some(parsed) = parse_reference(s) {
        let mut reference = Reference::from_name(parsed.name);
        reference.private_reference = parsed.private_reference;
        return Some((reference, parsed.create));
    }
    if let Some(name) = parse_attribute_reference(s) {
        let mut reference = Reference::from_name(name);
        reference.reference_type = ReferenceType::Attribute;
        return Some((reference, false));
    }
    None
}

/// Parses a style parent reference, which accepts more forms:
/// `@[[*]package:][style/]entry`, `?[[*]package:]style/entry`,
/// `[*package:][style/]entry`. Port of
/// `ResourceUtils::ParseStyleParentReference`.
pub fn parse_style_parent_reference(s: &str) -> Result<Option<Reference>, String> {
    if s.is_empty() {
        return Ok(None);
    }
    let mut name = s;
    let mut has_leading_identifiers = false;
    let mut private_ref = false;

    if name.starts_with('@') || name.starts_with('?') {
        has_leading_identifiers = true;
        name = &name[1..];
    }
    if let Some(rest) = name.strip_prefix('*') {
        private_ref = true;
        name = rest;
    }

    let Some((package, type_str, entry)) = extract_resource_name(name) else {
        return Err(format!("invalid parent reference '{s}'"));
    };
    if !type_str.is_empty() {
        match ResourceType::parse(type_str) {
            Some(ResourceType::Style) => {}
            _ => {
                return Err(format!(
                    "invalid resource type '{type_str}' for parent of style"
                ))
            }
        }
    }
    if !has_leading_identifiers && package.is_empty() && !type_str.is_empty() {
        return Err(format!("invalid parent reference '{s}'"));
    }

    let mut reference =
        Reference::from_name(ResourceName::new(package, ResourceType::Style, entry));
    reference.private_reference = private_ref;
    Ok(Some(reference))
}

/// Parses an XML attribute name `[*][package:]name` as a reference to
/// an `attr` resource. Port of `ResourceUtils::ParseXmlAttributeName`.
pub fn parse_xml_attribute_name(s: &str) -> Reference {
    let trimmed = trim_whitespace(s);
    let (private, rest) = match trimmed.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let (package, name) = match rest.find(':') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => ("", rest),
    };
    let entry = if name.is_empty() { trimmed } else { name };
    let mut reference = Reference::from_name(ResourceName::new(package, ResourceType::Attr, entry));
    reference.private_reference = private;
    reference
}

/// `@null` and `@empty`. Port of `ResourceUtils::TryParseNullOrEmpty`.
pub fn try_parse_null_or_empty(s: &str) -> Option<Item> {
    match trim_whitespace(s) {
        "@null" => Some(make_null()),
        "@empty" => Some(make_empty()),
        _ => None,
    }
}

/// `@null` flattens as `TYPE_REFERENCE` with data 0 (a `TYPE_NULL`/0
/// value is interpreted by the runtime as an error).
pub fn make_null() -> Item {
    Item::Reference(Reference::default())
}

pub fn make_empty() -> Item {
    Item::BinaryPrimitive(ResValue::new(res_value_type::TYPE_NULL, DATA_NULL_EMPTY))
}

/// Matches `s` against an enum attribute's symbols. Port of
/// `ResourceUtils::TryParseEnumSymbol`.
pub fn try_parse_enum_symbol(enum_attr: &Attribute, s: &str) -> Option<Item> {
    let trimmed = trim_whitespace(s);
    for symbol in &enum_attr.symbols {
        let name = symbol.symbol.name.as_ref()?;
        if trimmed == name.entry {
            return Some(Item::BinaryPrimitive(ResValue::new(symbol.data_type, symbol.value)));
        }
    }
    None
}

/// Matches `a|b|c` against a flag attribute's symbols. Port of
/// `ResourceUtils::TryParseFlagSymbol`.
pub fn try_parse_flag_symbol(flag_attr: &Attribute, s: &str) -> Option<Item> {
    let mut data = 0u32;
    if trim_whitespace(s).is_empty() {
        // The empty string is a valid flag set (0).
        return Some(Item::BinaryPrimitive(ResValue::new(res_value_type::TYPE_INT_HEX, 0)));
    }
    for part in s.split('|') {
        let trimmed = trim_whitespace(part);
        let mut matched = false;
        for symbol in &flag_attr.symbols {
            if let Some(name) = &symbol.symbol.name {
                if trimmed == name.entry {
                    data |= symbol.value;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            return None;
        }
    }
    Some(Item::BinaryPrimitive(ResValue::new(res_value_type::TYPE_INT_HEX, data)))
}

/// `#rgb`, `#argb`, `#rrggbb`, `#aarrggbb`.
/// Port of `ResourceUtils::TryParseColor`.
pub fn try_parse_color(s: &str) -> Option<Item> {
    use res_value_type::*;
    let color = trim_whitespace(s);
    let bytes = color.as_bytes();
    if bytes.is_empty() || bytes[0] != b'#' {
        return None;
    }
    let hex = |b: u8| -> Option<u32> { (b as char).to_digit(16) };
    let value = match bytes.len() {
        4 => {
            let (r, g, b) = (hex(bytes[1])?, hex(bytes[2])?, hex(bytes[3])?);
            ResValue::new(
                TYPE_INT_COLOR_RGB4,
                0xff00_0000 | r << 20 | r << 16 | g << 12 | g << 8 | b << 4 | b,
            )
        }
        5 => {
            let (a, r, g, b) = (hex(bytes[1])?, hex(bytes[2])?, hex(bytes[3])?, hex(bytes[4])?);
            ResValue::new(
                TYPE_INT_COLOR_ARGB4,
                a << 28 | a << 24 | r << 20 | r << 16 | g << 12 | g << 8 | b << 4 | b,
            )
        }
        7 => {
            let mut rgb = 0u32;
            for &b in &bytes[1..7] {
                rgb = (rgb << 4) | hex(b)?;
            }
            ResValue::new(TYPE_INT_COLOR_RGB8, 0xff00_0000 | rgb)
        }
        9 => {
            let mut argb = 0u32;
            for &b in &bytes[1..9] {
                argb = (argb << 4) | hex(b)?;
            }
            ResValue::new(TYPE_INT_COLOR_ARGB8, argb)
        }
        _ => return None,
    };
    Some(Item::BinaryPrimitive(value))
}

/// `true`/`false` (also `TRUE`/`True`…). Port of `ResourceUtils::ParseBool`.
pub fn parse_bool(s: &str) -> Option<bool> {
    match trim_whitespace(s) {
        "true" | "TRUE" | "True" => Some(true),
        "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

pub fn make_bool(value: bool) -> Item {
    Item::BinaryPrimitive(ResValue::new(
        res_value_type::TYPE_INT_BOOLEAN,
        if value { 0xffff_ffff } else { 0 },
    ))
}

pub fn try_parse_bool(s: &str) -> Option<Item> {
    parse_bool(s).map(make_bool)
}

/// Decimal or `0x` hex integer. Port of `android::ResTable::stringToInt`
/// (`U16StringToInt`).
pub fn string_to_int(s: &str) -> Option<ResValue> {
    let s = trim_whitespace(s);
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0usize;
    let neg = bytes[0] == b'-';
    if neg {
        i += 1;
    }
    if i >= bytes.len() || !bytes[i].is_ascii_digit() {
        return None;
    }

    let mut value: i64 = 0;
    let is_hex = bytes.len() > i + 1 && bytes[i] == b'0' && bytes[i + 1] == b'x';
    if is_hex {
        if neg {
            return None;
        }
        i += 2;
        if i == bytes.len() {
            return None;
        }
        while i < bytes.len() {
            let digit = (bytes[i] as char).to_digit(16)? as i64;
            value = value * 16 + digit;
            i += 1;
            if value > u32::MAX as i64 {
                return None;
            }
        }
    } else {
        while i < bytes.len() {
            if !bytes[i].is_ascii_digit() {
                return None;
            }
            value = value * 10 + (bytes[i] - b'0') as i64;
            i += 1;
            if (neg && -value < i32::MIN as i64) || (!neg && value > i32::MAX as i64) {
                return None;
            }
        }
    }
    if neg {
        value = -value;
    }
    Some(ResValue::new(
        if is_hex { res_value_type::TYPE_INT_HEX } else { res_value_type::TYPE_INT_DEC },
        value as u32,
    ))
}

pub fn try_parse_int(s: &str) -> Option<Item> {
    string_to_int(s).map(Item::BinaryPrimitive)
}

/// Parses `0xPPTTEEEE` into a valid resource ID.
/// Port of `ResourceUtils::ParseResourceId`.
pub fn parse_resource_id(s: &str) -> Option<ResourceId> {
    let value = string_to_int(trim_whitespace(s))?;
    if value.data_type == res_value_type::TYPE_INT_HEX {
        let id = ResourceId(value.data);
        if id.is_valid() {
            return Some(id);
        }
    }
    None
}

/// Parses an SDK version: integer, development codename, or
/// `codename.fingerprint`. Port of `ResourceUtils::ParseSdkVersion`.
pub fn parse_sdk_version(s: &str) -> Option<i32> {
    let trimmed = trim_whitespace(s);
    if let Some(value) = string_to_int(trimmed) {
        return Some(value.data as i32);
    }
    if let Some(version) = development_sdk_code_name_version(trimmed) {
        return Some(version);
    }
    let codename = trimmed.split('.').next().unwrap_or(trimmed);
    development_sdk_code_name_version(codename)
}

fn development_sdk_code_name_version(code_name: &str) -> Option<i32> {
    let hit = DEVELOPMENT_SDK_CODE_NAMES
        .iter()
        .find(|name| code_name.starts_with(*name))?;
    if code_name.len() == hit.len() {
        return Some(DEVELOPMENT_SDK_LEVEL);
    }
    if code_name.len() == hit.len() + PRIVACY_SANDBOX_SUFFIX.len()
        && code_name.ends_with(PRIVACY_SANDBOX_SUFFIX)
    {
        return Some(DEVELOPMENT_SDK_LEVEL);
    }
    None
}

/// Scans the longest leading `strtof`-style decimal float and returns
/// (value, rest-of-string). Mirrors `parseFloatingPoint`'s use of
/// `strtof`.
fn scan_float(s: &str) -> Option<(f32, &str)> {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    if !bytes[0].is_ascii_digit() && bytes[0] != b'.' && bytes[0] != b'-' && bytes[0] != b'+' {
        return None;
    }
    let mut end = 0usize;
    if bytes[end] == b'-' || bytes[end] == b'+' {
        end += 1;
    }
    let mut saw_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        saw_digit = true;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return None;
    }
    // Optional exponent.
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let mut exp_end = end + 1;
        if exp_end < bytes.len() && (bytes[exp_end] == b'-' || bytes[exp_end] == b'+') {
            exp_end += 1;
        }
        let mut exp_digits = false;
        while exp_end < bytes.len() && bytes[exp_end].is_ascii_digit() {
            exp_end += 1;
            exp_digits = true;
        }
        if exp_digits {
            end = exp_end;
        }
    }
    let value: f32 = s[..end].parse().ok()?;
    Some((value, &s[end..]))
}

/// Converts a float magnitude to the complex fixed-point encoding used
/// by dimensions and fractions. Mirrors the radix/mantissa selection in
/// `ResTable::stringToFloat`.
fn encode_complex(value: f32) -> u32 {
    let neg = value < 0.0;
    let value = if neg { -value } else { value };
    let bits = (value as f64 * (1 << 23) as f64 + 0.5) as u64;
    let (radix, shift) = if bits & 0x7f_ffff == 0 {
        (complex::RADIX_23P0, 23)
    } else if bits & 0xffff_ffff_ff80_0000 == 0 {
        (complex::RADIX_0P23, 0)
    } else if bits & 0xffff_ffff_8000_0000 == 0 {
        (complex::RADIX_8P15, 8)
    } else if bits & 0xffff_ff80_0000_0000 == 0 {
        (complex::RADIX_16P7, 16)
    } else {
        (complex::RADIX_23P0, 23)
    };
    let mut mantissa = ((bits >> shift) as u32) & complex::MANTISSA_MASK;
    if neg {
        mantissa = (mantissa.wrapping_neg()) & complex::MANTISSA_MASK;
    }
    (radix << complex::RADIX_SHIFT) | (mantissa << complex::MANTISSA_SHIFT)
}

/// Parses a float, dimension (`12dp`), or fraction (`25%`).
/// Port of `ResTable::stringToFloat`.
pub fn string_to_float(s: &str) -> Option<ResValue> {
    let (value, rest) = scan_float(s)?;

    let unit_str = rest.trim_end_matches(|c: char| c.is_ascii_whitespace());
    if !unit_str.is_empty() && !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        // A unit suffix: (name, dataType, unit, scale).
        let units: &[(&str, u8, u32, f32)] = &[
            ("px", res_value_type::TYPE_DIMENSION, complex::UNIT_PX, 1.0),
            ("dip", res_value_type::TYPE_DIMENSION, complex::UNIT_DIP, 1.0),
            ("dp", res_value_type::TYPE_DIMENSION, complex::UNIT_DIP, 1.0),
            ("sp", res_value_type::TYPE_DIMENSION, complex::UNIT_SP, 1.0),
            ("pt", res_value_type::TYPE_DIMENSION, complex::UNIT_PT, 1.0),
            ("in", res_value_type::TYPE_DIMENSION, complex::UNIT_IN, 1.0),
            ("mm", res_value_type::TYPE_DIMENSION, complex::UNIT_MM, 1.0),
            ("%", res_value_type::TYPE_FRACTION, complex::UNIT_FRACTION, 1.0 / 100.0),
            ("%p", res_value_type::TYPE_FRACTION, complex::UNIT_FRACTION_PARENT, 1.0 / 100.0),
        ];
        // Longest match wins ("%p" before "%", "dip" before "dp"-prefix
        // ambiguity is resolved by full-string equality).
        let (_, data_type, unit, scale) =
            units.iter().find(|(name, ..)| *name == unit_str)?;
        let scaled = value * scale;
        return Some(ResValue::new(
            *data_type,
            (unit << complex::UNIT_SHIFT) | encode_complex(scaled),
        ));
    }

    if rest.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty() {
        return Some(ResValue::new(res_value_type::TYPE_FLOAT, value.to_bits()));
    }
    None
}

pub fn try_parse_float(s: &str) -> Option<Item> {
    string_to_float(s).map(Item::BinaryPrimitive)
}

/// Maps a `Res_value` data type to the attribute format mask it
/// satisfies. Port of `ResourceUtils::AndroidTypeToAttributeTypeMask`.
pub fn android_type_to_attribute_type_mask(data_type: u8) -> u32 {
    use res_value_type::*;
    match data_type {
        TYPE_NULL | TYPE_REFERENCE | TYPE_ATTRIBUTE | TYPE_DYNAMIC_REFERENCE
        | TYPE_DYNAMIC_ATTRIBUTE => format::REFERENCE,
        TYPE_STRING => format::STRING,
        TYPE_FLOAT => format::FLOAT,
        TYPE_DIMENSION => format::DIMENSION,
        TYPE_FRACTION => format::FRACTION,
        TYPE_INT_DEC | TYPE_INT_HEX => format::INTEGER | format::ENUM | format::FLAGS,
        TYPE_INT_BOOLEAN => format::BOOLEAN,
        TYPE_INT_COLOR_ARGB8 | TYPE_INT_COLOR_RGB8 | TYPE_INT_COLOR_ARGB4
        | TYPE_INT_COLOR_RGB4 => format::COLOR,
        _ => 0,
    }
}

/// Successively tries to parse `value` as each resource type allowed by
/// `type_mask`. References found with `@+` notation are reported through
/// `on_create_reference`. Port of
/// `ResourceUtils::TryParseItemForAttribute` (mask overload).
pub fn try_parse_item_for_attribute(
    value: &str,
    type_mask: u32,
    mut on_create_reference: Option<&mut dyn FnMut(&ResourceName) -> bool>,
) -> Option<Item> {
    if let Some(item) = try_parse_null_or_empty(value) {
        return Some(item);
    }

    if let Some((mut reference, create)) = try_parse_reference(value) {
        reference.type_flags = Some(type_mask);
        if create {
            if let Some(callback) = on_create_reference.as_mut() {
                let name = reference.name.clone().unwrap();
                if !callback(&name) {
                    return None;
                }
            }
        }
        return Some(Item::Reference(reference));
    }

    if type_mask & format::COLOR != 0 {
        if let Some(item) = try_parse_color(value) {
            return Some(item);
        }
    }
    if type_mask & format::BOOLEAN != 0 {
        if let Some(item) = try_parse_bool(value) {
            return Some(item);
        }
    }
    if type_mask & format::INTEGER != 0 {
        if let Some(item) = try_parse_int(value) {
            return Some(item);
        }
    }
    let float_mask = format::FLOAT | format::DIMENSION | format::FRACTION;
    if type_mask & float_mask != 0 {
        if let Some(parsed) = string_to_float(value) {
            if type_mask & android_type_to_attribute_type_mask(parsed.data_type) != 0 {
                let may_only_be_float = type_mask & !float_mask == 0;
                let parsed_as_float = parsed.data_type == res_value_type::TYPE_FLOAT;
                if !may_only_be_float && parsed_as_float {
                    // Guard against precision loss: parse as double and
                    // accept the float only when they agree within 1.
                    let f = f32::from_bits(parsed.data);
                    if let Ok(d) = trim_whitespace(value).parse::<f64>() {
                        if (f as f64 - d).abs() < 1.0 {
                            return Some(Item::BinaryPrimitive(parsed));
                        }
                    }
                } else {
                    return Some(Item::BinaryPrimitive(parsed));
                }
            }
        }
    }
    None
}

/// Attribute-aware variant: also tries the attribute's enum/flag
/// symbols. Port of `TryParseItemForAttribute` (attribute overload).
pub fn try_parse_item_for_attribute_def(
    value: &str,
    attr: &Attribute,
    on_create_reference: Option<&mut dyn FnMut(&ResourceName) -> bool>,
) -> Option<Item> {
    let type_mask = attr.type_mask;
    if let Some(item) = try_parse_item_for_attribute(value, type_mask, on_create_reference) {
        return Some(item);
    }
    if type_mask & format::ENUM != 0 {
        if let Some(item) = try_parse_enum_symbol(attr, value) {
            return Some(item);
        }
    }
    if type_mask & format::FLAGS != 0 {
        if let Some(item) = try_parse_flag_symbol(attr, value) {
            return Some(item);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        let (reference, create) = try_parse_reference("@string/foo").unwrap();
        assert!(!create);
        let name = reference.name.unwrap();
        assert_eq!(name.ty.ty, ResourceType::String);
        assert_eq!(name.entry, "foo");

        let (reference, create) = try_parse_reference("@+id/foo").unwrap();
        assert!(create);
        assert_eq!(reference.name.unwrap().ty.ty, ResourceType::Id);

        // @+ only works for ids.
        assert!(try_parse_reference("@+string/foo").is_none());

        let (reference, _) = try_parse_reference("?android:attr/textColor").unwrap();
        assert_eq!(reference.reference_type, ReferenceType::Attribute);
        assert_eq!(reference.name.unwrap().package, "android");

        let (reference, _) = try_parse_reference("?colorAccent").unwrap();
        assert_eq!(reference.name.unwrap().entry, "colorAccent");

        let (reference, _) = try_parse_reference("@*android:string/hidden").unwrap();
        assert!(reference.private_reference);
    }

    #[test]
    fn colors() {
        use res_value_type::*;
        let cases = [
            ("#f00", TYPE_INT_COLOR_RGB4, 0xffff0000u32),
            ("#8f00", TYPE_INT_COLOR_ARGB4, 0x88ff0000),
            ("#112233", TYPE_INT_COLOR_RGB8, 0xff112233),
            ("#44112233", TYPE_INT_COLOR_ARGB8, 0x44112233),
        ];
        for (input, ty, data) in cases {
            match try_parse_color(input) {
                Some(Item::BinaryPrimitive(v)) => {
                    assert_eq!(v.data_type, ty, "{input}");
                    assert_eq!(v.data, data, "{input}");
                }
                other => panic!("{input}: {other:?}"),
            }
        }
        assert!(try_parse_color("#11223").is_none());
        assert!(try_parse_color("red").is_none());
    }

    #[test]
    fn integers() {
        assert_eq!(
            string_to_int("123").unwrap(),
            ResValue::new(res_value_type::TYPE_INT_DEC, 123)
        );
        assert_eq!(
            string_to_int("-1").unwrap(),
            ResValue::new(res_value_type::TYPE_INT_DEC, 0xffffffff)
        );
        assert_eq!(
            string_to_int("0xff").unwrap(),
            ResValue::new(res_value_type::TYPE_INT_HEX, 255)
        );
        assert!(string_to_int("-0x1").is_none());
        assert!(string_to_int("12a").is_none());
        assert!(string_to_int("0x").is_none());
        assert!(string_to_int("4294967296").is_none());
    }

    #[test]
    fn dimensions_match_kotlin_reference_bits() {
        // 16dp: mantissa 16 in 23p0 radix, unit DIP.
        let v = string_to_float("16dp").unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_DIMENSION);
        assert_eq!(v.data, (16 << 8) | complex::UNIT_DIP);

        // 0.5dp uses fractional radix.
        let v = string_to_float("0.5dp").unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_DIMENSION);
        assert_eq!(
            v.data & complex::RADIX_MASK << complex::RADIX_SHIFT,
            complex::RADIX_0P23 << complex::RADIX_SHIFT
        );

        // 25% is a fraction scaled by 1/100.
        let v = string_to_float("25%").unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_FRACTION);
        assert_eq!(v.data & complex::UNIT_MASK, complex::UNIT_FRACTION);

        let v = string_to_float("25%p").unwrap();
        assert_eq!(v.data & complex::UNIT_MASK, complex::UNIT_FRACTION_PARENT);

        // Plain float.
        let v = string_to_float("1.5").unwrap();
        assert_eq!(v.data_type, res_value_type::TYPE_FLOAT);
        assert_eq!(f32::from_bits(v.data), 1.5);

        assert!(string_to_float("16q").is_none());
        assert!(string_to_float("dp").is_none());
    }

    #[test]
    fn item_for_attribute_ordering() {
        // A string-typed attr never produces primitives.
        assert!(try_parse_item_for_attribute("true", format::STRING, None).is_none());
        // Bool accepted for boolean mask.
        match try_parse_item_for_attribute("true", format::BOOLEAN, None) {
            Some(Item::BinaryPrimitive(v)) => assert_eq!(v.data, 0xffffffff),
            other => panic!("{other:?}"),
        }
        // @null is accepted regardless of mask.
        assert!(try_parse_item_for_attribute("@null", format::STRING, None).is_some());
        // References match any mask.
        assert!(matches!(
            try_parse_item_for_attribute("@string/x", format::ANY, None),
            Some(Item::Reference(_))
        ));
    }

    #[test]
    fn style_parents() {
        let r = parse_style_parent_reference("@android:style/Theme").unwrap().unwrap();
        assert_eq!(r.name.as_ref().unwrap().package, "android");

        let r = parse_style_parent_reference("Theme.Base").unwrap().unwrap();
        assert_eq!(r.name.as_ref().unwrap().entry, "Theme.Base");
        assert_eq!(r.name.as_ref().unwrap().ty.ty, ResourceType::Style);

        // Type without leading @/? or package is invalid.
        assert!(parse_style_parent_reference("style/Theme").is_err());
        // Wrong type is an error.
        assert!(parse_style_parent_reference("@android:string/Theme").is_err());
    }

    #[test]
    fn sdk_versions() {
        assert_eq!(parse_sdk_version("21"), Some(21));
        assert_eq!(parse_sdk_version("Tiramisu"), Some(DEVELOPMENT_SDK_LEVEL));
        assert_eq!(parse_sdk_version("VanillaIceCream"), Some(DEVELOPMENT_SDK_LEVEL));
        assert_eq!(parse_sdk_version("NotACodename"), None);
    }
}
