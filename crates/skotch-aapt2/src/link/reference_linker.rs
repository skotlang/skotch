//! Reference linking: resolving symbolic references to resource IDs and
//! compiling XML attribute values against their attribute definitions.
//!
//! Port of `link/ReferenceLinker.{h,cpp}` and
//! `link/XmlReferenceLinker` (in `link/XmlReferenceLinker.cpp`).
//!
//! STUB IN PARTS: table linking handles references, styles, arrays,
//! plurals and attributes; the full port refines private-visibility
//! diagnostics and macro substitution.

use super::symbol_table::SymbolTable;
use crate::diag::Diagnostics;
use crate::res::table::ResourceTable;
use crate::res::utils::{parse_xml_attribute_name, try_parse_item_for_attribute_def};
use crate::res::value::{format, Item, ItemValue, Reference, Value, ValueKind};
use crate::res::{ResourceName, Source};
use crate::xml::{
    extract_package_from_namespace, AaptAttribute, Element, SCHEMA_AUTO, SCHEMA_TOOLS,
};
use anyhow::{bail, Result};

/// Fully qualifies and resolves one reference in place. Returns false
/// (and reports) when the symbol is missing. Mirrors
/// `ReferenceLinker::LinkReference`.
fn link_reference(
    reference: &mut Reference,
    symbols: &SymbolTable,
    compilation_package: &str,
    source: &Source,
    diag: &Diagnostics,
) -> bool {
    if reference.id.is_some() && reference.name.is_none() {
        // Already-numeric references pass through.
        return true;
    }
    let Some(name) = &mut reference.name else {
        return true;
    };
    if name.package.is_empty() {
        name.package = compilation_package.to_string();
    }
    match symbols.find_by_name(name) {
        Some(symbol) => {
            reference.id = symbol.id;
            if symbol.is_dynamic {
                reference.is_dynamic = true;
            }
            true
        }
        None => {
            diag.error_at(source.clone(), format!("resource {name} not found"));
            false
        }
    }
}

fn link_item(
    item: &mut Item,
    meta_source: &Source,
    symbols: &SymbolTable,
    compilation_package: &str,
    diag: &Diagnostics,
) -> bool {
    match item {
        Item::Reference(reference) => {
            link_reference(reference, symbols, compilation_package, meta_source, diag)
        }
        _ => true,
    }
}

/// Resolves every reference in the table. Mirrors
/// `ReferenceLinker::Consume`.
pub fn link_table_references(
    table: &mut ResourceTable,
    symbols: &SymbolTable,
    compilation_package: &str,
    diag: &Diagnostics,
) -> Result<()> {
    let mut error = false;
    for package in &mut table.packages {
        for ty in &mut package.types {
            for entry in &mut ty.entries {
                for config_value in entry
                    .values
                    .iter_mut()
                    .chain(entry.flag_disabled_values.iter_mut())
                {
                    let Some(value) = &mut config_value.value else {
                        continue;
                    };
                    let source = value.meta.source.clone();
                    if !link_value(value, &source, symbols, compilation_package, diag) {
                        error = true;
                    }
                }
            }
        }
    }
    if error {
        bail!("failed linking references");
    }
    Ok(())
}

fn link_value(
    value: &mut Value,
    source: &Source,
    symbols: &SymbolTable,
    compilation_package: &str,
    diag: &Diagnostics,
) -> bool {
    let mut ok = true;
    match &mut value.kind {
        ValueKind::Item(item) => {
            ok &= link_item(item, source, symbols, compilation_package, diag);
        }
        ValueKind::Style(style) => {
            if let Some(parent) = &mut style.parent {
                ok &= link_reference(parent, symbols, compilation_package, source, diag);
            }
            for entry in &mut style.entries {
                // Resolve the attribute key and validate the value
                // against the attribute's accepted formats.
                if entry.key.name.is_some() {
                    if let Some(name) = &mut entry.key.name {
                        if name.package.is_empty() {
                            name.package = compilation_package.to_string();
                        }
                    }
                    match symbols.find_by_name(entry.key.name.as_ref().unwrap()) {
                        Some(symbol) => {
                            entry.key.id = symbol.id;
                            if let Some(attr) = &symbol.attribute {
                                // Raw strings get re-parsed against the
                                // attribute's allowed types.
                                if let Item::RawString(raw) = &entry.value.item {
                                    let raw = raw.clone();
                                    match try_parse_item_for_attribute_def(&raw, attr, None) {
                                        Some(item) => entry.value.item = item,
                                        None => {
                                            if attr.type_mask & format::STRING != 0 {
                                                entry.value.item = Item::String {
                                                    value:
                                                        crate::util::process_string_preserve_spaces(
                                                            &raw,
                                                        ),
                                                    untranslatable_sections: vec![],
                                                };
                                            } else {
                                                diag.error_at(
                                                    source.clone(),
                                                    format!(
                                                        "invalid value '{raw}' for attribute {}; \
                                                         expected {}",
                                                        entry.key.name.as_ref().unwrap(),
                                                        crate::res::value::Attribute::mask_string(
                                                            attr.type_mask
                                                        )
                                                    ),
                                                );
                                                ok = false;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            diag.error_at(
                                source.clone(),
                                format!(
                                    "style attribute '{}' not found",
                                    entry.key.name.as_ref().unwrap()
                                ),
                            );
                            ok = false;
                        }
                    }
                }
                ok &= link_item(
                    &mut entry.value.item,
                    source,
                    symbols,
                    compilation_package,
                    diag,
                );
            }
        }
        ValueKind::Attribute(attr) => {
            for symbol in &mut attr.symbols {
                ok &= link_reference(
                    &mut symbol.symbol,
                    symbols,
                    compilation_package,
                    source,
                    diag,
                );
            }
        }
        ValueKind::Styleable(styleable) => {
            for entry in &mut styleable.entries {
                ok &= link_reference(entry, symbols, compilation_package, source, diag);
            }
        }
        ValueKind::Array(array) => {
            for element in &mut array.elements {
                ok &= link_item(
                    &mut element.item,
                    source,
                    symbols,
                    compilation_package,
                    diag,
                );
            }
        }
        ValueKind::Plural(plural) => {
            for slot in plural.values.iter_mut().flatten() {
                ok &= link_item(&mut slot.item, source, symbols, compilation_package, diag);
            }
        }
        ValueKind::Macro(_) => {}
    }
    ok
}

/// Links an XML document: resolves attribute names to their attribute
/// definitions/IDs and compiles attribute values. Mirrors
/// `XmlReferenceLinker::Consume`.
pub fn link_xml_references(
    root: &mut Element,
    symbols: &SymbolTable,
    compilation_package: &str,
    source_path: &str,
    diag: &Diagnostics,
) -> Result<()> {
    let mut error = false;
    let mut namespace_stack: Vec<(String, String)> = Vec::new();
    link_element(
        root,
        symbols,
        compilation_package,
        source_path,
        &mut namespace_stack,
        &mut error,
        diag,
    );
    if error {
        bail!("failed linking XML references");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn link_element(
    element: &mut Element,
    symbols: &SymbolTable,
    compilation_package: &str,
    source_path: &str,
    namespace_stack: &mut Vec<(String, String)>,
    error: &mut bool,
    diag: &Diagnostics,
) {
    let pushed = element.namespace_decls.len();
    for decl in &element.namespace_decls {
        namespace_stack.push((decl.prefix.clone(), decl.uri.clone()));
    }

    let source = Source::with_line(source_path, element.line_number);
    for attr in &mut element.attributes {
        if attr.namespace_uri == SCHEMA_TOOLS {
            continue;
        }
        // Resolve the attribute name against its package namespace.
        if !attr.namespace_uri.is_empty() {
            let Some(extracted) = extract_package_from_namespace(&attr.namespace_uri) else {
                continue; // Unknown namespace: leave as raw string.
            };
            let package = if extracted.package.is_empty() {
                compilation_package.to_string()
            } else {
                extracted.package.clone()
            };
            let attr_name = ResourceName::new(package, crate::res::ResourceType::Attr, &attr.name);
            match symbols.find_by_name(&attr_name) {
                Some(symbol) => {
                    let attr_def = symbol
                        .attribute
                        .clone()
                        .unwrap_or_else(|| crate::res::value::Attribute::new(format::ANY));
                    // Compile the value against the attribute formats.
                    if attr_def.type_mask != format::STRING || attr.value.starts_with('@') {
                        attr.compiled_value =
                            try_parse_item_for_attribute_def(&attr.value, &attr_def, None);
                    }
                    if let Some(Item::Reference(reference)) = &mut attr.compiled_value {
                        let mut linked = reference.clone();
                        if !link_reference(&mut linked, symbols, compilation_package, &source, diag)
                        {
                            *error = true;
                        }
                        attr.compiled_value = Some(Item::Reference(linked));
                    }
                    if attr.compiled_value.is_none() && attr_def.type_mask & format::STRING == 0 {
                        diag.error_at(
                            source.clone(),
                            format!(
                                "invalid value for attribute '{}': '{}' (must be {})",
                                attr.name,
                                attr.value,
                                crate::res::value::Attribute::mask_string(attr_def.type_mask)
                            ),
                        );
                        *error = true;
                    }
                    attr.compiled_attribute = Some(AaptAttribute {
                        attribute: attr_def,
                        id: symbol.id,
                    });
                }
                None => {
                    diag.error_at(
                        source.clone(),
                        format!(
                            "attribute {} not found",
                            parse_xml_attribute_name(&format!(
                                "{}:{}",
                                extracted.package, attr.name
                            ))
                        ),
                    );
                    *error = true;
                }
            }
        } else if attr.value.starts_with('@') || attr.value.starts_with('?') {
            // No namespace: still compile plain references (e.g.
            // android:icon="@drawable/x" handled above; here things
            // like style="@style/Foo").
            if let Some((mut reference, _)) = crate::res::utils::try_parse_reference(&attr.value) {
                if link_reference(&mut reference, symbols, compilation_package, &source, diag) {
                    attr.compiled_value = Some(Item::Reference(reference));
                } else {
                    *error = true;
                }
            }
        }
    }

    for child in element.child_elements_mut() {
        link_element(
            child,
            symbols,
            compilation_package,
            source_path,
            namespace_stack,
            error,
            diag,
        );
    }
    namespace_stack.truncate(namespace_stack.len() - pushed);
}

// Keep ItemValue referenced for the full port's signature stability.
#[allow(dead_code)]
fn _types(_: &ItemValue, _: &SCHEMA_AUTO_TYPE) {}
#[allow(non_camel_case_types)]
type SCHEMA_AUTO_TYPE = ();
const _: &str = SCHEMA_AUTO;
