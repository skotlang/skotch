//! Value rendering for the dump printers.
//!
//! Ports the `PrettyPrint`/`Print` member functions of
//! `ResourceValues.cpp` (and the printf float formatting they rely on)
//! so `dump resources`, `dump xmltree`, `dump apc`, `dump chunks`, and
//! `diff` render values byte-for-byte like aapt2.

use super::printer::Printer;
use crate::res::value::{
    res_value_type, Attribute, FileType, Item, Plural, Reference, ReferenceType, ResValue, Style,
    Value, ValueKind, PLURAL_COUNT,
};

/// `printf("%e", f)`-compatible formatting (two-digit signed exponent).
fn printf_e(value: f64, precision: usize) -> String {
    let s = format!("{value:.precision$e}");
    match s.split_once('e') {
        Some((mantissa, exp)) => {
            let exp: i32 = exp.parse().unwrap_or(0);
            let sign = if exp < 0 { '-' } else { '+' };
            format!("{mantissa}e{sign}{:02}", exp.abs())
        }
        None => s,
    }
}

/// `printf("%g", f)`-compatible formatting (default precision 6).
fn printf_g(value: f32) -> String {
    let v = value as f64;
    if v == 0.0 {
        return "0".to_string();
    }
    // Decimal exponent as %e would compute it (after rounding to 5 places).
    let e_repr = format!("{v:.5e}");
    let exp: i32 = e_repr
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);
    let formatted = if exp < -4 || exp >= 6 {
        let with_exp = printf_e(v, 5);
        let (mantissa, exp_part) = with_exp.split_once('e').unwrap();
        let mantissa = strip_trailing_zeros(mantissa);
        format!("{mantissa}e{exp_part}")
    } else {
        let precision = (5 - exp).max(0) as usize;
        strip_trailing_zeros(&format!("{v:.precision$}")).to_string()
    };
    formatted
}

fn strip_trailing_zeros(s: &str) -> &str {
    if !s.contains('.') {
        return s;
    }
    let s = s.trim_end_matches('0');
    s.strip_suffix('.').unwrap_or(s)
}

/// Port of `BinaryPrimitive::DecideFormat` + the `%…` dispatch in
/// `BinaryPrimitive::PrettyPrint` for `TYPE_FLOAT`.
fn format_float(f: f32) -> String {
    if f.abs() as f64 > i64::MAX as f64 || (f != 0.0 && f.abs() < 1e-10) {
        printf_e(f as f64, 6)
    } else if f.is_finite() && f as i64 as f32 == f {
        format!("{f:.0}")
    } else {
        printf_g(f)
    }
}

/// Port of `ComplexToString` (dimension/fraction rendering).
fn complex_to_string(complex_value: u32, fraction: bool) -> String {
    const RADIX_SHIFTS: [u32; 4] = [23, 16, 8, 0];
    let radix = ((complex_value >> 4) & 0x3) as usize;
    let mantissa = (((complex_value >> 8) & 0xffffff) as u64) << RADIX_SHIFTS[radix];
    let value = mantissa as f32 * (1.0f32 / (1 << 23) as f32);
    let mut str = format!("{value:.6}");
    let unit_type = complex_value & 0xf;
    if fraction {
        str.push_str(match unit_type {
            0 => "%",
            1 => "%p",
            _ => "???",
        });
    } else {
        str.push_str(match unit_type {
            0 => "px",
            1 => "dp",
            2 => "sp",
            3 => "pt",
            4 => "in",
            5 => "mm",
            _ => "???",
        });
    }
    str
}

/// Port of `BinaryPrimitive::PrettyPrint`.
pub fn pretty_print_primitive(value: &ResValue, printer: &mut Printer) {
    use res_value_type::*;
    match value.data_type {
        TYPE_NULL => {
            if value.data == crate::res::value::DATA_NULL_EMPTY {
                printer.print("@empty");
            } else {
                printer.print("@null");
            }
        }
        TYPE_INT_DEC => {
            printer.print(format!("{}", value.data as i32));
        }
        TYPE_INT_HEX => {
            printer.print(format!("0x{:08x}", value.data));
        }
        TYPE_INT_BOOLEAN => {
            printer.print(if value.data != 0 { "true" } else { "false" });
        }
        TYPE_INT_COLOR_ARGB8 | TYPE_INT_COLOR_RGB8 | TYPE_INT_COLOR_ARGB4 | TYPE_INT_COLOR_RGB4 => {
            printer.print(format!("#{:08x}", value.data));
        }
        TYPE_FLOAT => {
            printer.print(format_float(f32::from_bits(value.data)));
        }
        TYPE_DIMENSION => {
            printer.print(complex_to_string(value.data, false));
        }
        TYPE_FRACTION => {
            printer.print(complex_to_string(value.data, true));
        }
        _ => {
            printer.print(format!(
                "(unknown 0x{:02x}) 0x{:08x}",
                value.data_type, value.data
            ));
        }
    }
}

/// Port of `PrettyPrintReferenceImpl`.
pub fn pretty_print_reference_impl(reference: &Reference, print_package: bool, printer: &mut Printer) {
    match reference.reference_type {
        ReferenceType::Resource => printer.print("@"),
        ReferenceType::Attribute => printer.print("?"),
    };

    if reference.name.is_none() && reference.id.is_none() {
        printer.print("null");
        return;
    }

    if reference.private_reference {
        printer.print("*");
    }

    if let Some(name) = &reference.name {
        if print_package {
            printer.print(name.to_string());
        } else {
            printer.print(name.ty.to_string());
            printer.print("/");
            printer.print(&name.entry);
        }
    } else if let Some(id) = reference.id {
        if id.is_valid() {
            printer.print(id.to_string());
        }
    }
}

/// Port of `Reference::PrettyPrint(StringPiece package, Printer*)`:
/// suppresses the package when it matches `package`.
pub fn pretty_print_reference(reference: &Reference, package: &str, printer: &mut Printer) {
    let print_package = match &reference.name {
        Some(name) => package != name.package,
        None => true,
    };
    pretty_print_reference_impl(reference, print_package, printer);
}

/// `Item`/`Value` PrettyPrint dispatch. Items without a dedicated
/// `PrettyPrint` override fall back to their `Print(std::ostream)`
/// rendering, matching `Value::PrettyPrint`.
pub fn pretty_print_item(item: &Item, printer: &mut Printer) {
    match item {
        Item::Reference(r) => pretty_print_reference_impl(r, true, printer),
        Item::String { value, .. } => {
            printer.print("\"");
            printer.print(value);
            printer.print("\"");
        }
        Item::BinaryPrimitive(rv) => pretty_print_primitive(rv, printer),
        other => {
            printer.print(item_print_string(other));
        }
    }
}

/// Like [`pretty_print_item`] but suppresses the package of references
/// matching `package` (used by the table printers).
pub fn pretty_print_item_in_package(item: &Item, package: &str, printer: &mut Printer) {
    match item {
        Item::Reference(r) => pretty_print_reference(r, package, printer),
        other => pretty_print_item(other, printer),
    }
}

/// Port of `Reference::Print(std::ostream)`.
fn reference_print_string(reference: &Reference) -> String {
    let mut out = String::new();
    if reference.reference_type == ReferenceType::Resource {
        out.push_str("(reference) @");
        if reference.name.is_none() && reference.id.is_none() {
            out.push_str("null");
            return out;
        }
    } else {
        out.push_str("(attr-reference) ?");
    }
    if reference.private_reference {
        out.push('*');
    }
    if let Some(name) = &reference.name {
        out.push_str(&name.to_string());
    }
    if let Some(id) = reference.id {
        if id.is_valid() {
            if reference.name.is_some() {
                out.push(' ');
            }
            out.push_str(&id.to_string());
        }
    }
    out
}

/// Port of the `Print(std::ostream)` member functions of the `Item`
/// subclasses (used by `diff` and as the PrettyPrint fallback).
pub fn item_print_string(item: &Item) -> String {
    match item {
        Item::Reference(r) => reference_print_string(r),
        Item::Id => "(id)".to_string(),
        Item::RawString(s) => format!("(raw string) {s}"),
        Item::String { value, .. } => format!("(string) \"{value}\""),
        Item::StyledString { value, spans, .. } => {
            let mut out = format!("(styled string) \"{value}\"");
            for span in spans {
                out.push_str(&format!(" {}:{},{}", span.name, span.first_char, span.last_char));
            }
            out
        }
        Item::FileReference(f) => {
            let mut out = format!("(file) {}", f.path);
            match f.file_type {
                FileType::BinaryXml => out.push_str(" type=XML"),
                FileType::ProtoXml => out.push_str(" type=protoXML"),
                FileType::Png => out.push_str(" type=PNG"),
                FileType::Unknown => {}
            }
            out
        }
        Item::BinaryPrimitive(rv) => {
            format!("(primitive) type=0x{:02x} data=0x{:08x}", rv.data_type, rv.data)
        }
    }
}

fn attribute_print_string(attr: &Attribute) -> String {
    let mut out = format!("(attr) {}", Attribute::mask_string(attr.type_mask));
    if !attr.symbols.is_empty() {
        let symbols: Vec<String> = attr
            .symbols
            .iter()
            .map(|s| {
                let name = match &s.symbol.name {
                    Some(n) => n.entry.clone(),
                    None => "???".to_string(),
                };
                format!("{name}={}", s.value)
            })
            .collect();
        out.push_str(&format!(" [{}]", symbols.join(", ")));
    }
    if attr.min_int != i32::MIN {
        out.push_str(&format!(" min={}", attr.min_int));
    }
    if attr.max_int != i32::MAX {
        out.push_str(&format!(" max={}", attr.max_int));
    }
    out
}

fn style_print_string(style: &Style) -> String {
    let mut out = "(style) ".to_string();
    if let Some(parent) = &style.parent {
        if let Some(name) = &parent.name {
            if parent.private_reference {
                out.push('*');
            }
            out.push_str(&name.to_string());
        }
    }
    // `operator<<(out, Style::Entry)`: key name (or id, or ???),
    // " = ", then the item.
    let entries: Vec<String> = style
        .entries
        .iter()
        .map(|e| {
            let key = match (&e.key.name, e.key.id) {
                (Some(name), _) => name.to_string(),
                (None, Some(id)) => id.to_string(),
                (None, None) => "???".to_string(),
            };
            format!("{key} = {}", item_print_string(&e.value.item))
        })
        .collect();
    out.push_str(&format!(" [{}]", entries.join(", ")));
    out
}

fn plural_print_string(plural: &Plural) -> String {
    let mut out = "(plural)".to_string();
    for i in 0..PLURAL_COUNT {
        if let Some(v) = &plural.values[i] {
            out.push_str(&format!(
                " {}={}",
                crate::res::value::plural_arity_name(i),
                item_print_string(&v.item)
            ));
        }
    }
    out
}

/// Port of `Value::Print(std::ostream)` across the value hierarchy
/// (used by `diff` to describe mismatched values).
pub fn value_print_string(value: &Value) -> String {
    match &value.kind {
        ValueKind::Item(item) => item_print_string(item),
        ValueKind::Attribute(attr) => attribute_print_string(attr),
        ValueKind::Style(style) => style_print_string(style),
        ValueKind::Styleable(styleable) => {
            let entries: Vec<String> =
                styleable.entries.iter().map(reference_print_string).collect();
            format!("(styleable)  [{}]", entries.join(", "))
        }
        ValueKind::Array(array) => {
            let elements: Vec<String> =
                array.elements.iter().map(|e| item_print_string(&e.item)).collect();
            format!("(array) [{}]", elements.join(", "))
        }
        ValueKind::Plural(plural) => plural_print_string(plural),
        ValueKind::Macro(_) => "(macro) ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pretty(rv: ResValue) -> String {
        let mut out = String::new();
        let mut p = Printer::new(&mut out);
        pretty_print_primitive(&rv, &mut p);
        out
    }

    #[test]
    fn float_formats_match_printf() {
        use res_value_type::TYPE_FLOAT;
        assert_eq!(pretty(ResValue::new(TYPE_FLOAT, 1.0f32.to_bits())), "1");
        assert_eq!(pretty(ResValue::new(TYPE_FLOAT, 0.5f32.to_bits())), "0.5");
        assert_eq!(pretty(ResValue::new(TYPE_FLOAT, 1e-11f32.to_bits())), "1.000000e-11");
        assert_eq!(pretty(ResValue::new(TYPE_FLOAT, 3.14f32.to_bits())), "3.14");
    }

    #[test]
    fn dimension_format() {
        use res_value_type::TYPE_DIMENSION;
        // 16dp: mantissa 16 << 8, radix 23p0, unit dip.
        let data = (16u32 << 8) | 1;
        assert_eq!(pretty(ResValue::new(TYPE_DIMENSION, data)), "16.000000dp");
    }
}
