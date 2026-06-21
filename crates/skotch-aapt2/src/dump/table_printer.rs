//! Resource-table printers.
//!
//! Ports `Debug::PrintTable`, `Debug::DumpOverlayable`,
//! `Debug::PrintStyleGraph`, and `Debug::DumpResStringPool` from
//! `Debug.cpp`.

use super::printer::Printer;
use super::values::pretty_print_item_in_package;
use crate::res::string_pool::BinaryStringPool;
use crate::res::table::{ResourceTable, VisibilityLevel};
use crate::res::value::{format as attr_format, Attribute, Value, ValueKind};
use crate::res::{ResourceId, ResourceName, ResourceType};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct DebugPrintTableOptions {
    pub show_sources: bool,
    pub show_values: bool,
}

/// Headline (one-line) rendering of a value (`ValueHeadlinePrinter`).
fn print_value_headline(value: &Value, package: &str, printer: &mut Printer) {
    match &value.kind {
        ValueKind::Attribute(attr) => {
            printer.print("(attr) type=");
            printer.print(Attribute::mask_string(attr.type_mask));
            if !attr.symbols.is_empty() {
                printer.print(format!(" size={}", attr.symbols.len()));
            }
        }
        ValueKind::Style(style) => {
            printer.print(format!("(style) size={}", style.entries.len()));
            if let Some(parent_ref) = &style.parent {
                printer.print(" parent=");
                if let Some(parent_name) = &parent_ref.name {
                    if parent_ref.private_reference {
                        printer.print("*");
                    }
                    if package != parent_name.package {
                        printer.print(&parent_name.package);
                        printer.print(":");
                    }
                    printer.print(parent_name.ty.to_string());
                    printer.print("/");
                    printer.print(&parent_name.entry);
                    if let Some(id) = parent_ref.id {
                        printer.print(" (");
                        printer.print(id.to_string());
                        printer.print(")");
                    }
                } else if let Some(id) = parent_ref.id {
                    printer.print(id.to_string());
                } else {
                    printer.print("???");
                }
            }
        }
        ValueKind::Array(array) => {
            printer.print(format!("(array) size={}", array.elements.len()));
        }
        ValueKind::Plural(plural) => {
            let count = plural.values.iter().filter(|v| v.is_some()).count();
            printer.print(format!("(plurals) size={count}"));
        }
        ValueKind::Styleable(styleable) => {
            printer.println(format!("(styleable) size={}", styleable.entries.len()));
        }
        ValueKind::Item(item) => {
            pretty_print_item_in_package(item, package, printer);
        }
        ValueKind::Macro(_) => {
            printer.print("(macro)");
        }
    }
}

/// Multi-line body rendering of a value (`ValueBodyPrinter`).
fn print_value_body(value: &Value, package: &str, printer: &mut Printer) {
    match &value.kind {
        ValueKind::Attribute(attr) => {
            let mask = attr_format::ENUM | attr_format::FLAGS;
            if attr.type_mask & mask != 0 {
                for symbol in &attr.symbols {
                    if let Some(name) = &symbol.symbol.name {
                        printer.print(&name.entry);
                        if let Some(id) = symbol.symbol.id {
                            printer.print("(");
                            printer.print(id.to_string());
                            printer.print(")");
                        }
                    } else if let Some(id) = symbol.symbol.id {
                        printer.print(id.to_string());
                    } else {
                        printer.print("???");
                    }
                    printer.println(format!("=0x{:08x}", symbol.value));
                }
            }
        }
        ValueKind::Style(style) => {
            for entry in &style.entries {
                if let Some(name) = &entry.key.name {
                    if !name.package.is_empty() && name.package != package {
                        printer.print(&name.package);
                        printer.print(":");
                    }
                    printer.print(&name.entry);
                    if let Some(id) = entry.key.id {
                        printer.print("(");
                        printer.print(id.to_string());
                        printer.print(")");
                    }
                } else if let Some(id) = entry.key.id {
                    printer.print(id.to_string());
                } else {
                    printer.print("???");
                }
                printer.print("=");
                pretty_print_item_in_package(&entry.value.item, package, printer);
                printer.println_empty();
            }
        }
        ValueKind::Array(array) => {
            let count = array.elements.len();
            printer.print("[");
            for (i, element) in array.elements.iter().enumerate() {
                if i != 0 && i % 4 == 0 {
                    printer.println_empty();
                    printer.print(" ");
                }
                pretty_print_item_in_package(&element.item, package, printer);
                if i != count - 1 {
                    printer.print(", ");
                }
            }
            printer.println("]");
        }
        ValueKind::Plural(plural) => {
            for (i, value) in plural.values.iter().enumerate() {
                if let Some(value) = value {
                    printer.print(format!("{}=", crate::res::value::plural_arity_name(i)));
                    pretty_print_item_in_package(&value.item, package, printer);
                    printer.println_empty();
                }
            }
        }
        ValueKind::Styleable(styleable) => {
            for attr in &styleable.entries {
                if let Some(name) = &attr.name {
                    if !name.package.is_empty() && name.package != package {
                        printer.print(&name.package);
                        printer.print(":");
                    }
                    printer.print(&name.entry);
                    if let Some(id) = attr.id {
                        printer.print("(");
                        printer.print(id.to_string());
                        printer.print(")");
                    }
                }
                if let Some(id) = attr.id {
                    printer.print(id.to_string());
                }
                printer.println_empty();
            }
        }
        ValueKind::Item(_) | ValueKind::Macro(_) => {}
    }
}

/// Port of `Debug::PrintTable`.
pub fn print_table(table: &ResourceTable, options: DebugPrintTableOptions, printer: &mut Printer) {
    for (package, types) in table.sorted_view() {
        if !table.included_packages.is_empty() {
            printer.println(format!(
                "DynamicRefTable entryCount={}",
                table.included_packages.len()
            ));
            printer.indent();
            for (id, name) in &table.included_packages {
                printer.println(format!("0x{id:02x} -> {name}"));
            }
            printer.undent();
        }

        // Derive the package/type IDs from the first assigned entry ID,
        // mirroring the partitioned view.
        let package_id: Option<u8> = package
            .types
            .iter()
            .flat_map(|t| t.entries.iter())
            .find_map(|e| e.id.map(|id| id.package_id()));

        printer.print("Package name=");
        printer.print(&package.name);
        if let Some(id) = package_id {
            printer.print(format!(" id={id:02x}"));
        }
        printer.println_empty();

        printer.indent();
        for (ty, entries) in types {
            let type_id: Option<u8> = ty.entries.iter().find_map(|e| e.id.map(|id| id.type_id()));
            printer.print("type ");
            printer.print(ty.named_type.to_string());
            if let Some(id) = type_id {
                printer.print(format!(" id={id:02x}"));
            }
            printer.println(format!(" entryCount={}", entries.len()));

            printer.indent();
            for entry in entries {
                printer.print("resource ");
                printer.print(
                    ResourceId::new(
                        package_id.unwrap_or(0),
                        type_id.unwrap_or(0),
                        entry.id.map(|id| id.entry_id()).unwrap_or(0),
                    )
                    .to_string(),
                );
                printer.print(" ");
                printer.print(ty.named_type.to_string());
                printer.print("/");
                printer.print(&entry.name);

                match entry.visibility.level {
                    VisibilityLevel::Public => {
                        printer.print(" PUBLIC");
                    }
                    VisibilityLevel::Private => {
                        printer.print(" _PRIVATE_");
                    }
                    VisibilityLevel::Undefined => {}
                }
                if entry.visibility.staged_api {
                    printer.print(" STAGED");
                }
                if entry.overlayable_item.is_some() {
                    printer.print(" OVERLAYABLE");
                }
                if let Some(staged_id) = entry.staged_id {
                    printer.print(" STAGED_ID=");
                    printer.print(staged_id.id.to_string());
                }
                printer.println_empty();

                if options.show_values {
                    printer.indent();
                    for value in &entry.values {
                        let Some(val) = &value.value else { continue };
                        printer.print("(");
                        printer.print(value.config.to_string());
                        printer.print(") ");
                        print_value_headline(val, &package.name, printer);
                        if options.show_sources && !val.meta.source.path.is_empty() {
                            printer.print(" src=");
                            printer.print(val.meta.source.to_string());
                        }
                        printer.println_empty();
                        printer.indent();
                        print_value_body(val, &package.name, printer);
                        printer.undent();
                    }
                    printer.println("Flag disabled values:");
                    for value in &entry.flag_disabled_values {
                        let Some(val) = &value.value else { continue };
                        printer.print("(");
                        printer.print(value.config.to_string());
                        printer.print(") ");
                        print_value_headline(val, &package.name, printer);
                        if options.show_sources && !val.meta.source.path.is_empty() {
                            printer.print(" src=");
                            printer.print(val.meta.source.to_string());
                        }
                        printer.println_empty();
                        printer.indent();
                        print_value_body(val, &package.name, printer);
                        printer.undent();
                    }
                    printer.undent();
                }
            }
            printer.undent();
        }
        printer.undent();
    }
}

/// Port of `Debug::DumpOverlayable`.
pub fn dump_overlayable(table: &ResourceTable, printer: &mut Printer) {
    let mut items: Vec<(String, String, String)> = Vec::new();
    for package in &table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                if let Some(overlayable_item) = &entry.overlayable_item {
                    let overlayable = table
                        .overlayables
                        .get(overlayable_item.overlayable_index)
                        .cloned()
                        .unwrap_or_default();
                    let overlayable_section = format!(
                        "name=\"{}\" actor=\"{}\"",
                        overlayable.name, overlayable.actor
                    );
                    let policy_subsection = format!(
                        "policies=\"{}\"",
                        policies_to_debug_string(overlayable_item.policies)
                    );
                    let value = format!("{}/{}", ty.named_type, entry.name);
                    items.push((overlayable_section, policy_subsection, value));
                }
            }
        }
    }
    items.sort();

    let mut last_overlayable_section = String::new();
    let mut last_policy_subsection = String::new();
    for (overlayable_section, policy_subsection, resource_name) in items {
        if last_overlayable_section != overlayable_section {
            printer.println(&overlayable_section);
            last_overlayable_section = overlayable_section;
        }
        if last_policy_subsection != policy_subsection {
            printer.indent();
            printer.println(&policy_subsection);
            last_policy_subsection = policy_subsection;
            printer.undent();
        }
        printer.indent();
        printer.indent();
        printer.println(resource_name);
        printer.undent();
        printer.undent();
    }
}

/// Port of `android::idmap2::policy::PoliciesToDebugString`.
pub fn policies_to_debug_string(policies: u32) -> String {
    use crate::res::table::policy;
    const POLICY_STRING_TO_FLAG: &[(&str, u32)] = &[
        ("actor", policy::ACTOR_SIGNATURE),
        ("odm", policy::ODM_PARTITION),
        ("oem", policy::OEM_PARTITION),
        ("product", policy::PRODUCT_PARTITION),
        ("public", policy::PUBLIC),
        ("config_signature", policy::CONFIG_SIGNATURE),
        ("signature", policy::SIGNATURE),
        ("system", policy::SYSTEM_PARTITION),
        ("vendor", policy::VENDOR_PARTITION),
    ];
    let mut str = String::new();
    let mut remaining = policies;
    for (name, flag) in POLICY_STRING_TO_FLAG {
        if policies & flag != *flag {
            continue;
        }
        if !str.is_empty() {
            str.push('|');
        }
        str.push_str(name);
        remaining &= !flag;
    }
    if remaining != 0 {
        if !str.is_empty() {
            str.push('|');
        }
        str.push_str(&format!("0x{remaining:08x}"));
    }
    if str.is_empty() {
        "none".to_string()
    } else {
        str
    }
}

/// Port of `Debug::PrintStyleGraph`.
pub fn print_style_graph(table: &ResourceTable, target_style: ResourceName, printer: &mut Printer) {
    let mut graph: BTreeMap<ResourceName, BTreeSet<ResourceName>> = BTreeMap::new();
    let mut styles_to_visit: VecDeque<ResourceName> = VecDeque::new();
    styles_to_visit.push_back(target_style);
    while let Some(style_name) = styles_to_visit.pop_front() {
        if graph
            .get(&style_name)
            .is_some_and(|parents| !parents.is_empty())
        {
            // Already visited.
            continue;
        }
        let parents = graph.entry(style_name.clone()).or_default();
        if let Some(result) = table.find_resource(&style_name) {
            for value in &result.entry.values {
                if let Some(Value {
                    kind: ValueKind::Style(style),
                    ..
                }) = &value.value
                {
                    if let Some(parent) = &style.parent {
                        if let Some(parent_name) = &parent.name {
                            parents.insert(parent_name.clone());
                            styles_to_visit.push_back(parent_name.clone());
                        }
                    }
                }
            }
        }
    }

    let names: Vec<&ResourceName> = graph.keys().collect();
    let node_index = |name: &ResourceName| {
        names
            .binary_search_by(|probe| probe.cmp(&name))
            .unwrap_or(0)
    };

    printer.print("digraph styles {\n");
    for name in &names {
        printer.print(format!("  node_{} [label=\"{name}\"];\n", node_index(name)));
    }
    for (style_name, parents) in &graph {
        let style_node_index = node_index(style_name);
        for parent_name in parents {
            printer.print(format!(
                "  node_{style_node_index} -> node_{};\n",
                node_index(parent_name)
            ));
        }
    }
    printer.print("}\n");
}

/// Builds the target style name for `dump styleparents`.
pub fn style_resource_name(package: &str, style: &str) -> ResourceName {
    ResourceName::new(package, ResourceType::Style, style)
}

/// Port of `Debug::DumpResStringPool`, applied to a parsed binary
/// string pool chunk (`chunk_size` is the pool's byte size).
pub fn dump_res_string_pool(pool: &BinaryStringPool, chunk_size: usize, printer: &mut Printer) {
    let mut unique: BTreeSet<String> = BTreeSet::new();
    let count = pool.len();
    for i in 0..count {
        unique.insert(pool.get(i).unwrap_or_default());
    }
    printer.print(format!(
        "String pool of {} unique {} {} strings, {} entries and {} styles using {} bytes:\n",
        unique.len(),
        if pool.is_utf8() { "UTF-8" } else { "UTF-16" },
        if pool.is_sorted() {
            "sorted"
        } else {
            "non-sorted"
        },
        count,
        pool.style_count(),
        chunk_size
    ));
    for i in 0..count {
        printer.print(format!(
            "String #{i} : {}\n",
            pool.get(i).unwrap_or_default()
        ));
    }
}

/// Finds the first string-pool chunk inside a chunk stream that starts
/// at `offset` (used for `dump strings`/`dump xmlstrings`). Returns the
/// pool chunk bytes.
pub fn find_string_pool_chunk(data: &[u8], mut offset: usize) -> Option<&[u8]> {
    const RES_STRING_POOL_TYPE: u16 = 0x0001;
    while offset + 8 <= data.len() {
        let chunk_type = u16::from_le_bytes(data.get(offset..offset + 2)?.try_into().ok()?);
        let chunk_size =
            u32::from_le_bytes(data.get(offset + 4..offset + 8)?.try_into().ok()?) as usize;
        if chunk_size < 8 || offset + chunk_size > data.len() {
            return None;
        }
        if chunk_type == RES_STRING_POOL_TYPE {
            return Some(&data[offset..offset + chunk_size]);
        }
        offset += chunk_size;
    }
    None
}
