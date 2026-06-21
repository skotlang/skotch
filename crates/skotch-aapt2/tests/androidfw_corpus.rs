//! Validation against the androidfw test corpus: real APKs built by
//! aapt/aapt2 and checked into AOSP at `libs/androidfw/tests/data`.
//! These are the same artifacts Android's own runtime tests parse, so
//! they are ground truth for the binary formats — including shapes our
//! own outputs never produce (sparse encoding, out-of-order type
//! chunks, shared libraries, aapt1-era encodings).
//!
//! Three layers of checks:
//! 1. parse: known resource IDs/values from the fixtures' `R.h` and
//!    `res/` sources must surface through our `resources.arsc` parser;
//! 2. round-trip: parse → our `TableFlattener` → parse must preserve
//!    the logical table; same for `AndroidManifest.xml` through the
//!    XML flattener;
//! 3. robustness: the malformed fixtures (`bad.apk`,
//!    `length_decode_invalid.apk`) must not panic.
//!
//! All tests skip silently when the AOSP checkout is absent.

use skotch_aapt2::binary::arsc_flattener::{flatten_table, TableFlattenerOptions};
use skotch_aapt2::binary::arsc_parser::parse_table;
use skotch_aapt2::res::table::{policy, ResourceTable};
use skotch_aapt2::res::value::{res_value_type, Item, ValueKind};
use skotch_aapt2::res::{ResourceId, ResourceName, ResourceType};
use skotch_aapt2::xml::axml::parse_binary_xml;
use skotch_aapt2::xml::flatten::{flatten_xml, XmlFlattenerOptions};
use skotch_aapt2::xml::{Element, Node};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};

fn data_root() -> Option<PathBuf> {
    let path = PathBuf::from("/opt/src/github/skotlang/android/base/libs/androidfw/tests/data");
    path.is_dir().then_some(path)
}

fn zip_entry(apk: &Path, name: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(apk).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name(name).ok()?;
    let mut data = Vec::new();
    entry.read_to_end(&mut data).ok()?;
    Some(data)
}

fn apk_table(apk: &Path) -> ResourceTable {
    let arsc = zip_entry(apk, "resources.arsc")
        .unwrap_or_else(|| panic!("{}: no resources.arsc", apk.display()));
    parse_table(&arsc).unwrap_or_else(|e| panic!("{}: {e}", apk.display()))
}

fn find<'t>(
    table: &'t ResourceTable,
    package: &str,
    ty: ResourceType,
    entry: &str,
) -> skotch_aapt2::res::table::SearchResult<'t> {
    table
        .find_resource(&ResourceName::new(package, ty, entry))
        .unwrap_or_else(|| panic!("{package}:{ty}/{entry} not found"))
}

fn default_item<'e>(entry: &'e skotch_aapt2::res::table::ResourceEntry) -> &'e Item {
    let value = entry
        .values
        .iter()
        .find(|cv| cv.config.is_default())
        .and_then(|cv| cv.value.as_ref())
        .unwrap_or_else(|| panic!("{}: no default value", entry.name));
    match &value.kind {
        ValueKind::Item(item) => item,
        other => panic!("{}: not an item: {other:?}", entry.name),
    }
}

// ───────────────────── logical signatures ─────────────────────

fn item_signature(item: &Item) -> String {
    match item {
        Item::Reference(r) => format!(
            "ref:{:08x}:{}",
            r.id.map(|id| id.0).unwrap_or(0),
            match r.reference_type {
                skotch_aapt2::res::value::ReferenceType::Resource => "r",
                skotch_aapt2::res::value::ReferenceType::Attribute => "a",
            }
        ),
        Item::Id => "id".to_string(),
        Item::RawString(s) => format!("raw:{s}"),
        Item::String { value, .. } => format!("str:{value}"),
        Item::StyledString { value, spans, .. } => {
            let mut out = format!("styled:{value}");
            for span in spans {
                let _ = write!(out, ";{}@{}-{}", span.name, span.first_char, span.last_char);
            }
            out
        }
        Item::FileReference(f) => format!("file:{}", f.path),
        Item::BinaryPrimitive(v) => format!("bin:{:02x}:{:08x}", v.data_type, v.data),
    }
}

fn value_signature(value: &skotch_aapt2::res::value::Value) -> String {
    match &value.kind {
        ValueKind::Item(item) => item_signature(item),
        ValueKind::Attribute(attr) => {
            let mut symbols: Vec<String> = attr
                .symbols
                .iter()
                .map(|s| {
                    format!(
                        "{}={:08x}",
                        s.symbol
                            .name
                            .as_ref()
                            .map(|n| n.entry.clone())
                            .unwrap_or_default(),
                        s.value
                    )
                })
                .collect();
            symbols.sort();
            format!(
                "attr:mask={:08x}:min={}:max={}:[{}]",
                attr.type_mask,
                attr.min_int,
                attr.max_int,
                symbols.join(",")
            )
        }
        ValueKind::Style(style) => {
            let mut entries: Vec<String> = style
                .entries
                .iter()
                .map(|e| {
                    format!(
                        "{:08x}={}",
                        e.key.id.map(|id| id.0).unwrap_or(0),
                        item_signature(&e.value.item)
                    )
                })
                .collect();
            entries.sort();
            format!(
                "style:parent={:08x}:[{}]",
                style
                    .parent
                    .as_ref()
                    .and_then(|p| p.id)
                    .map(|id| id.0)
                    .unwrap_or(0),
                entries.join(",")
            )
        }
        ValueKind::Styleable(s) => format!("styleable:{}", s.entries.len()),
        ValueKind::Array(array) => {
            let items: Vec<String> = array
                .elements
                .iter()
                .map(|e| item_signature(&e.item))
                .collect();
            format!("array:[{}]", items.join(","))
        }
        ValueKind::Plural(plural) => {
            let mut out = String::from("plural:");
            for (index, slot) in plural.values.iter().enumerate() {
                if let Some(item) = slot {
                    let _ = write!(out, "{index}={};", item_signature(&item.item));
                }
            }
            out
        }
        ValueKind::Macro(_) => "macro".to_string(),
    }
}

/// One line per (resource, config, value) plus entry metadata, so two
/// logically equal tables produce identical sets.
fn table_signature(table: &ResourceTable) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for package in &table.packages {
        for ty in &package.types {
            for entry in &ty.entries {
                let id = entry.id.map(|id| id.0).unwrap_or(0);
                let visibility = format!("{:?}", entry.visibility.level);
                if let Some(item) = &entry.overlayable_item {
                    let overlayable = table
                        .overlayables
                        .get(item.overlayable_index)
                        .map(|o| o.name.clone())
                        .unwrap_or_default();
                    out.insert(format!(
                        "OVERLAYABLE|{}|{}/{}|{overlayable}|{:04x}",
                        package.name, ty.named_type, entry.name, item.policies
                    ));
                }
                for config_value in &entry.values {
                    let Some(value) = &config_value.value else {
                        continue;
                    };
                    out.insert(format!(
                        "RES|{}|{}/{}|{id:08x}|{visibility}|{}|{}",
                        package.name,
                        ty.named_type,
                        entry.name,
                        config_value.config,
                        value_signature(value)
                    ));
                }
            }
        }
    }
    // Self-referential library entries are canonical noise: aapt2's
    // TableFlattener emits a DynamicRefTable self-mapping for every
    // package with a non-standard ID (not 0x01/0x7f), while aapt1-built
    // originals omit it and the runtime adds it implicitly. Only
    // mappings to *other* packages are signature-relevant.
    for (id, name) in &table.included_packages {
        if table.packages.iter().any(|p| &p.name == name) {
            continue;
        }
        out.insert(format!("LIB|{id:02x}|{name}"));
    }
    out
}

fn xml_signature(element: &Element, out: &mut Vec<String>, depth: usize) {
    let mut decls: Vec<String> = element
        .namespace_decls
        .iter()
        .map(|d| format!("{}={}", d.prefix, d.uri))
        .collect();
    decls.sort();
    let mut attrs: Vec<String> = element
        .attributes
        .iter()
        .map(|a| {
            let value = match &a.compiled_value {
                Some(Item::String { value, .. }) => format!("str:{value}"),
                Some(item) => item_signature(item),
                None => format!("raw:{}", a.value),
            };
            format!(
                "{}|{}|{}|{:08x}",
                a.namespace_uri,
                a.name,
                value,
                a.compiled_attribute
                    .as_ref()
                    .and_then(|c| c.id)
                    .map(|id| id.0)
                    .unwrap_or(0)
            )
        })
        .collect();
    attrs.sort();
    out.push(format!(
        "{depth}|{}|{}|ns[{}]|at[{}]",
        element.namespace_uri,
        element.name,
        decls.join(";"),
        attrs.join(";")
    ));
    for child in &element.children {
        match child {
            Node::Element(el) => xml_signature(el, out, depth + 1),
            Node::Text(text) => {
                if !text.text.trim().is_empty() {
                    out.push(format!("{depth}|#text|{}", text.text));
                }
            }
        }
    }
}

// ───────────────────── parse-side assertions ─────────────────────

#[test]
fn basic_apk_ids_and_values() {
    let Some(root) = data_root() else { return };
    let table = apk_table(&root.join("basic/basic.apk"));
    let package = "com.android.basic";

    // IDs from basic/R.h.
    let test1 = find(&table, package, ResourceType::String, "test1");
    assert_eq!(test1.entry.id, Some(ResourceId(0x7f030000)));
    match default_item(test1.entry) {
        Item::String { value, .. } => assert_eq!(value, "test1"),
        other => panic!("test1: {other:?}"),
    }
    assert_eq!(
        test1.entry.visibility.level,
        skotch_aapt2::res::table::VisibilityLevel::Public
    );

    // <integer name="number1">200</integer>
    let number1 = find(&table, package, ResourceType::Integer, "number1");
    assert_eq!(number1.entry.id, Some(ResourceId(0x7f040000)));
    match default_item(number1.entry) {
        Item::BinaryPrimitive(v) => {
            assert_eq!(v.data_type, res_value_type::TYPE_INT_DEC);
            assert_eq!(v.data, 200);
        }
        other => panic!("number1: {other:?}"),
    }

    // <integer name="ref1">@integer/ref2</integer>
    let ref1 = find(&table, package, ResourceType::Integer, "ref1");
    match default_item(ref1.entry) {
        Item::Reference(r) => assert_eq!(r.id, Some(ResourceId(0x7f040003))),
        other => panic!("ref1: {other:?}"),
    }

    // Theme2 inherits Theme1 and overrides attr1 with 300.
    let theme2 = find(&table, package, ResourceType::Style, "Theme2");
    let theme2_value = theme2.entry.values[0].value.as_ref().unwrap();
    match &theme2_value.kind {
        ValueKind::Style(style) => {
            assert_eq!(
                style.parent.as_ref().and_then(|p| p.id),
                Some(ResourceId(0x7f050000))
            );
            assert_eq!(style.entries.len(), 1);
            assert_eq!(style.entries[0].key.id, Some(ResourceId(0x7f010000)));
            match &style.entries[0].value.item {
                Item::BinaryPrimitive(v) => assert_eq!(v.data, 300),
                other => panic!("Theme2 entry: {other:?}"),
            }
        }
        other => panic!("Theme2: {other:?}"),
    }

    // integerArray1 = [1, 2, 3].
    let array = find(&table, package, ResourceType::Array, "integerArray1");
    match &array.entry.values[0].value.as_ref().unwrap().kind {
        ValueKind::Array(a) => {
            let data: Vec<u32> = a
                .elements
                .iter()
                .map(|e| match &e.item {
                    Item::BinaryPrimitive(v) => v.data,
                    other => panic!("array element: {other:?}"),
                })
                .collect();
            assert_eq!(data, vec![1, 2, 3]);
        }
        other => panic!("integerArray1: {other:?}"),
    }

    // attr1 declares format reference|integer.
    let attr1 = find(&table, package, ResourceType::Attr, "attr1");
    match &attr1.entry.values[0].value.as_ref().unwrap().kind {
        ValueKind::Attribute(attr) => {
            use skotch_aapt2::res::value::format;
            assert_eq!(
                attr.type_mask & (format::REFERENCE | format::INTEGER),
                format::REFERENCE | format::INTEGER
            );
        }
        other => panic!("attr1: {other:?}"),
    }
}

#[test]
fn basic_locale_and_density_variants() {
    let Some(root) = data_root() else { return };
    let package = "com.android.basic";

    // basic_de_fr.apk carries German and French strings.
    let table = apk_table(&root.join("basic/basic_de_fr.apk"));
    let test1 = find(&table, package, ResourceType::String, "test1");
    let configs: BTreeSet<String> = test1
        .entry
        .values
        .iter()
        .map(|cv| cv.config.to_string())
        .collect();
    assert!(configs.contains("de"), "{configs:?}");
    assert!(configs.contains("fr"), "{configs:?}");

    // Density splits define @string/density per dpi bucket.
    for (apk, qualifier) in [
        ("basic/basic_hdpi-v4.apk", "hdpi-v4"),
        ("basic/basic_xhdpi-v4.apk", "xhdpi-v4"),
        ("basic/basic_xxhdpi-v4.apk", "xxhdpi-v4"),
    ] {
        let table = apk_table(&root.join(apk));
        let density = find(&table, package, ResourceType::String, "density");
        let configs: BTreeSet<String> = density
            .entry
            .values
            .iter()
            .map(|cv| cv.config.to_string())
            .collect();
        assert!(configs.contains(qualifier), "{apk}: {configs:?}");
    }
}

#[test]
fn sparse_apk_matches_not_sparse() {
    let Some(root) = data_root() else { return };
    let sparse = apk_table(&root.join("sparse/sparse.apk"));
    let not_sparse = apk_table(&root.join("sparse/not_sparse.apk"));

    // The sparse encoding is a pure container optimization; both APKs
    // were linked from the same inputs and must decode identically.
    assert_eq!(table_signature(&sparse), table_signature(&not_sparse));

    // Spot-check entries from sparse/R.h.
    let package = "com.android.sparse";
    let foo0 = find(&sparse, package, ResourceType::Integer, "foo_0");
    assert_eq!(foo0.entry.id, Some(ResourceId(0x7f010000)));
    let foo999 = find(&sparse, package, ResourceType::String, "foo_999");
    assert_eq!(foo999.entry.id, Some(ResourceId(0x7f0203e7)));
}

#[test]
fn shared_libraries_and_dynamic_references() {
    let Some(root) = data_root() else { return };

    // Shared libraries are built with package ID 0x00, assigned at
    // runtime via the library (DynamicRefTable) chunk.
    let lib_one = apk_table(&root.join("lib_one/lib_one.apk"));
    let lib_package = &lib_one.packages[0];
    assert_eq!(lib_package.name, "com.android.lib_one");
    let attr1 = find(&lib_one, "com.android.lib_one", ResourceType::Attr, "attr1");
    let id = attr1.entry.id.expect("attr1 id");
    assert_eq!(id.package_id(), 0x00, "shared lib uses package ID 0");
    assert_eq!(id.type_id(), 0x01);
    // aapt1-built shared libraries carry no library chunk at all; the
    // self-mapping is implicit in package ID 0x00 (the runtime adds it
    // when loading). Verified by chunk inspection: lib_one.apk has only
    // string-pool/typeSpec/type chunks inside the package.

    // The client references both libraries through its dynamic ref table.
    let libclient = apk_table(&root.join("libclient/libclient.apk"));
    let names: BTreeSet<&str> = libclient
        .included_packages
        .iter()
        .map(|(_, n)| n.as_str())
        .collect();
    assert!(names.contains("com.android.lib_one"), "{names:?}");
    assert!(names.contains("com.android.lib_two"), "{names:?}");
}

#[test]
fn overlayable_declarations_parse() {
    let Some(root) = data_root() else { return };
    let table = apk_table(&root.join("overlayable/overlayable.apk"));
    let package = "com.android.overlayable";

    assert!(
        table
            .overlayables
            .iter()
            .any(|o| o.name == "OverlayableResources1" && o.actor == "overlay://theme"),
        "{:?}",
        table.overlayables
    );

    // <policy type="product|system"><item string/overlayable2></policy>
    let overlayable2 = find(&table, package, ResourceType::String, "overlayable2");
    let item = overlayable2
        .entry
        .overlayable_item
        .as_ref()
        .expect("overlayable2 item");
    assert_eq!(
        item.policies & (policy::PRODUCT_PARTITION | policy::SYSTEM_PARTITION),
        policy::PRODUCT_PARTITION | policy::SYSTEM_PARTITION,
        "policies: {:#x}",
        item.policies
    );

    // <policy type="public"> covers overlayable1 and overlayable4.
    for name in ["overlayable1", "overlayable4"] {
        let entry = find(&table, package, ResourceType::String, name);
        let item = entry
            .entry
            .overlayable_item
            .as_ref()
            .unwrap_or_else(|| panic!("{name}"));
        assert!(
            item.policies & policy::PUBLIC != 0,
            "{name}: {:#x}",
            item.policies
        );
    }

    // not_overlayable has no overlayable declaration.
    let plain = find(&table, package, ResourceType::String, "not_overlayable");
    assert!(plain.entry.overlayable_item.is_none());
}

// ───────────────────── round-trip checks ─────────────────────

/// Every well-formed corpus APK with a resource table.
const ROUND_TRIP_APKS: &[&str] = &[
    "app/app.apk",
    "appaslib/appaslib.apk",
    "basic/basic.apk",
    "basic/basic_de_fr.apk",
    "basic/basic_hdpi-v4.apk",
    "basic/basic_xhdpi-v4.apk",
    "basic/basic_xxhdpi-v4.apk",
    "feature/feature.apk",
    "lib_one/lib_one.apk",
    "lib_two/lib_two.apk",
    "libclient/libclient.apk",
    "out_of_order_types/out_of_order_types.apk",
    "overlay/overlay.apk",
    "overlayable/overlayable.apk",
    "sparse/not_sparse.apk",
    "sparse/sparse.apk",
    "styles/styles.apk",
];

#[test]
fn corpus_tables_round_trip_through_flattener() {
    let Some(root) = data_root() else { return };
    let mut failures = Vec::new();
    for relative in ROUND_TRIP_APKS {
        let path = root.join(relative);
        if !path.exists() {
            continue;
        }
        let original = apk_table(&path);
        let flattened = match flatten_table(&original, &TableFlattenerOptions::default()) {
            Ok(bytes) => bytes,
            Err(e) => {
                failures.push(format!("{relative}: flatten failed: {e}"));
                continue;
            }
        };
        let reparsed = match parse_table(&flattened) {
            Ok(table) => table,
            Err(e) => {
                failures.push(format!("{relative}: reparse failed: {e}"));
                continue;
            }
        };
        let before = table_signature(&original);
        let after = table_signature(&reparsed);
        if before != after {
            let missing: Vec<&String> = before.difference(&after).take(5).collect();
            let added: Vec<&String> = after.difference(&before).take(5).collect();
            failures.push(format!(
                "{relative}: signature mismatch\n  lost:  {missing:#?}\n  added: {added:#?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn corpus_manifests_round_trip_through_xml_flattener() {
    let Some(root) = data_root() else { return };
    let mut failures = Vec::new();
    for relative in ROUND_TRIP_APKS {
        let path = root.join(relative);
        let Some(manifest_bytes) = zip_entry(&path, "AndroidManifest.xml") else {
            continue;
        };
        let original = match parse_binary_xml(&manifest_bytes) {
            Ok(root) => root,
            Err(e) => {
                failures.push(format!("{relative}: manifest parse failed: {e}"));
                continue;
            }
        };
        let flattened = flatten_xml(
            &original,
            &XmlFlattenerOptions {
                keep_raw_values: false,
                use_utf16: true,
            },
        );
        let reparsed = match parse_binary_xml(&flattened) {
            Ok(root) => root,
            Err(e) => {
                failures.push(format!("{relative}: manifest reparse failed: {e}"));
                continue;
            }
        };
        let mut before = Vec::new();
        xml_signature(&original, &mut before, 0);
        let mut after = Vec::new();
        xml_signature(&reparsed, &mut after, 0);
        if before != after {
            let diff: Vec<String> = before
                .iter()
                .zip(after.iter())
                .filter(|(b, a)| b != a)
                .take(3)
                .map(|(b, a)| format!("  before: {b}\n  after:  {a}"))
                .collect();
            failures.push(format!(
                "{relative}: manifest XML signature mismatch ({} vs {} nodes)\n{}",
                before.len(),
                after.len(),
                diff.join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn loader_bare_arsc_parses() {
    let Some(root) = data_root() else { return };
    let path = root.join("loader/resources.arsc");
    if !path.exists() {
        return;
    }
    let data = std::fs::read(&path).unwrap();
    let table = parse_table(&data).expect("loader resources.arsc parses");
    assert!(!table.packages.is_empty());
}

// ───────────────────── robustness ─────────────────────

#[test]
fn malformed_corpus_inputs_do_not_panic() {
    let Some(root) = data_root() else { return };

    // bad.apk: intentionally corrupt.
    if let Some(data) = zip_entry(&root.join("bad/bad.apk"), "resources.arsc") {
        let _ = parse_table(&data); // any Result is fine; panic is not
    }

    // length_decode_invalid.apk has a string with a corrupt encoded
    // length; the parser must reject or tolerate it without panicking.
    if let Some(data) = zip_entry(
        &root.join("length_decode/length_decode_invalid.apk"),
        "resources.arsc",
    ) {
        let _ = parse_table(&data);
    }

    // The valid twin must parse.
    if let Some(data) = zip_entry(
        &root.join("length_decode/length_decode_valid.apk"),
        "resources.arsc",
    ) {
        parse_table(&data).expect("length_decode_valid parses");
    }

    // Manifests of the malformed APKs, too.
    for relative in ["bad/bad.apk", "length_decode/length_decode_invalid.apk"] {
        if let Some(data) = zip_entry(&root.join(relative), "AndroidManifest.xml") {
            let _ = parse_binary_xml(&data);
        }
    }
}
