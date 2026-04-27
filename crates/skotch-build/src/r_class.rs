//! R class generation for Android resource compilation.
//!
//! Scans the `res/` directory structure and generates `R.java`-compatible
//! class bytes containing integer constants for each resource. The generated
//! class is added to the compilation classpath so Kotlin code can reference
//! `R.string.app_name`, `R.layout.activity_main`, etc.
//!
//! # Resource ID Layout
//!
//! Following Android's convention:
//! - Bits 24-31: package ID (0x7f for app resources)
//! - Bits 16-23: type ID (1=attr, 2=drawable, 3=layout, 4=string, etc.)
//! - Bits 0-15: entry ID (sequential within type)

use std::collections::BTreeMap;
use std::path::Path;

/// A discovered resource with its assigned ID.
#[derive(Clone, Debug)]
pub struct ResourceEntry {
    pub name: String,
    pub id: u32,
}

/// All discovered resources organized by type.
#[derive(Clone, Debug, Default)]
pub struct ResourceTable {
    pub entries: BTreeMap<String, Vec<ResourceEntry>>,
}

/// Standard Android resource type IDs.
fn type_id_for(res_type: &str) -> u8 {
    match res_type {
        "attr" => 1,
        "drawable" => 2,
        "mipmap" => 3,
        "layout" => 4,
        "anim" => 5,
        "animator" => 6,
        "color" => 7,
        "dimen" => 8,
        "string" => 9,
        "style" => 10,
        "menu" => 11,
        "xml" => 12,
        "raw" => 13,
        "font" => 14,
        "navigation" => 15,
        _ => 16,
    }
}

/// Scan a `res/` directory and build the resource table.
///
/// Discovers resources from directory structure:
/// - `res/layout/activity_main.xml` → R.layout.activity_main
/// - `res/drawable/icon.png` → R.drawable.icon
/// - `res/values/strings.xml` → parses <string name="app_name"> → R.string.app_name
pub fn scan_resources(res_dir: &Path) -> ResourceTable {
    let mut table = ResourceTable::default();
    if !res_dir.is_dir() {
        return table;
    }

    for entry in std::fs::read_dir(res_dir).into_iter().flatten().flatten() {
        let dir_name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        if !entry.path().is_dir() {
            continue;
        }
        // Resource type is the directory name without qualifier suffix.
        // e.g. "layout-land" → "layout", "drawable-hdpi" → "drawable"
        let res_type = dir_name.split('-').next().unwrap_or(&dir_name).to_string();

        if res_type == "values" {
            // Parse XML files in values/ for named resources.
            parse_values_dir(&entry.path(), &mut table);
        } else {
            // Each file in the directory is a resource.
            for file_entry in std::fs::read_dir(entry.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if file_entry.path().is_file() {
                    let name = file_entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        table
                            .entries
                            .entry(res_type.clone())
                            .or_default()
                            .push(ResourceEntry { name, id: 0 });
                    }
                }
            }
        }
    }

    // Assign IDs.
    assign_resource_ids(&mut table);
    table
}

/// Parse values/ XML files for named resources (string, color, dimen, style, etc.)
fn parse_values_dir(values_dir: &Path, table: &mut ResourceTable) {
    for entry in std::fs::read_dir(values_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("xml") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            parse_values_xml(&content, table);
        }
    }
}

/// Minimal XML parser for values files.
/// Extracts `<string name="xxx">`, `<color name="xxx">`, `<dimen name="xxx">`, etc.
fn parse_values_xml(xml: &str, table: &mut ResourceTable) {
    // Look for patterns like: <string name="app_name">...</string>
    for line in xml.lines() {
        let line = line.trim();
        for tag in &[
            "string", "color", "dimen", "style", "bool", "integer", "array",
        ] {
            let prefix = format!("<{tag} name=\"");
            if let Some(rest) = line.strip_prefix(&prefix) {
                if let Some(end) = rest.find('"') {
                    let name = rest[..end].to_string();
                    table
                        .entries
                        .entry(tag.to_string())
                        .or_default()
                        .push(ResourceEntry { name, id: 0 });
                }
            }
        }
    }
}

/// Assign numeric IDs to all resources following Android conventions.
fn assign_resource_ids(table: &mut ResourceTable) {
    const PACKAGE_ID: u32 = 0x7f;
    for (res_type, entries) in &mut table.entries {
        let type_id = type_id_for(res_type) as u32;
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.id = (PACKAGE_ID << 24) | (type_id << 16) | (i as u32 + 1);
        }
    }
}

/// Generate R class bytecode as a JVM .class file.
///
/// Produces a class with nested static classes for each resource type:
/// ```text
/// public final class R {
///     public static final class string {
///         public static final int app_name = 0x7f090001;
///     }
/// }
/// ```
///
/// Returns `Vec<(class_name, bytes)>` — one entry for the outer R class
/// and one for each inner type class.
pub fn generate_r_class(package: &str, table: &ResourceTable) -> Vec<(String, Vec<u8>)> {
    let r_class_name = if package.is_empty() {
        "R".to_string()
    } else {
        format!("{}/R", package.replace('.', "/"))
    };

    let mut classes = Vec::new();

    // For each resource type, generate an inner class with int constants.
    for (res_type, entries) in &table.entries {
        let inner_name = format!("{}${}", r_class_name, res_type);
        let class_bytes = generate_inner_r_class(&inner_name, entries);
        classes.push((inner_name, class_bytes));
    }

    // Generate the outer R class (just a marker with no fields).
    let outer_bytes = generate_outer_r_class(&r_class_name, table);
    classes.push((r_class_name, outer_bytes));

    classes
}

/// Generate bytecode for an inner R class (e.g., R$string) with int constants.
fn generate_inner_r_class(class_name: &str, entries: &[ResourceEntry]) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut buf = Vec::new();

    // Magic
    buf.write_u32::<BigEndian>(0xCAFEBABE).unwrap();
    // Version: Java 17 (class file version 61.0)
    buf.write_u16::<BigEndian>(0).unwrap(); // minor
    buf.write_u16::<BigEndian>(61).unwrap(); // major

    // Constant pool — we need entries for:
    // 1. class name (this), 2. super class (Object),
    // 3. field names + descriptors, 4. ConstantValue attributes
    let mut cp: Vec<Vec<u8>> = vec![
        Vec::new(),                     // index 0 is unused
        utf8_entry(class_name),         // #1: this class name
        class_entry(1),                 // #2: this class
        utf8_entry("java/lang/Object"), // #3: super name
        class_entry(3),                 // #4: super class
        utf8_entry("I"),                // #5: field descriptor
        utf8_entry("ConstantValue"),    // #6: attribute name
    ];

    // For each field: field_name (Utf8) + int constant value
    let mut field_name_indices = Vec::new();
    let mut const_value_indices = Vec::new();
    for entry in entries {
        let name_idx = cp.len() as u16;
        cp.push(utf8_entry(&entry.name));
        field_name_indices.push(name_idx);

        let val_idx = cp.len() as u16;
        cp.push(int_entry(entry.id as i32));
        const_value_indices.push(val_idx);
    }

    let cp_count = cp.len() as u16;
    buf.write_u16::<BigEndian>(cp_count).unwrap();
    for entry in &cp[1..] {
        buf.extend_from_slice(entry);
    }

    // Access flags: ACC_PUBLIC | ACC_FINAL | ACC_SUPER
    buf.write_u16::<BigEndian>(0x0031).unwrap();
    // This class
    buf.write_u16::<BigEndian>(2).unwrap();
    // Super class
    buf.write_u16::<BigEndian>(4).unwrap();
    // Interfaces count
    buf.write_u16::<BigEndian>(0).unwrap();

    // Fields
    buf.write_u16::<BigEndian>(entries.len() as u16).unwrap();
    for i in 0..entries.len() {
        // ACC_PUBLIC | ACC_STATIC | ACC_FINAL
        buf.write_u16::<BigEndian>(0x0019).unwrap();
        // name index
        buf.write_u16::<BigEndian>(field_name_indices[i]).unwrap();
        // descriptor index ("I")
        buf.write_u16::<BigEndian>(5).unwrap();
        // 1 attribute: ConstantValue
        buf.write_u16::<BigEndian>(1).unwrap();
        // ConstantValue attribute
        buf.write_u16::<BigEndian>(6).unwrap(); // attribute_name_index
        buf.write_u32::<BigEndian>(2).unwrap(); // attribute_length
        buf.write_u16::<BigEndian>(const_value_indices[i]).unwrap(); // constantvalue_index
    }

    // Methods count (0 — no methods)
    buf.write_u16::<BigEndian>(0).unwrap();
    // Attributes count (0)
    buf.write_u16::<BigEndian>(0).unwrap();

    buf
}

/// Generate bytecode for the outer R class.
fn generate_outer_r_class(class_name: &str, _table: &ResourceTable) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut buf = Vec::new();

    buf.write_u32::<BigEndian>(0xCAFEBABE).unwrap();
    buf.write_u16::<BigEndian>(0).unwrap();
    buf.write_u16::<BigEndian>(61).unwrap();

    // Minimal constant pool: this + super
    let cp_count = 5u16;
    buf.write_u16::<BigEndian>(cp_count).unwrap();
    // #1: class name
    buf.extend_from_slice(&utf8_entry(class_name));
    // #2: this class
    buf.extend_from_slice(&class_entry(1));
    // #3: "java/lang/Object"
    buf.extend_from_slice(&utf8_entry("java/lang/Object"));
    // #4: super class
    buf.extend_from_slice(&class_entry(3));

    // Access flags: ACC_PUBLIC | ACC_FINAL | ACC_SUPER
    buf.write_u16::<BigEndian>(0x0031).unwrap();
    buf.write_u16::<BigEndian>(2).unwrap(); // this
    buf.write_u16::<BigEndian>(4).unwrap(); // super
    buf.write_u16::<BigEndian>(0).unwrap(); // interfaces
    buf.write_u16::<BigEndian>(0).unwrap(); // fields
    buf.write_u16::<BigEndian>(0).unwrap(); // methods
    buf.write_u16::<BigEndian>(0).unwrap(); // attributes

    buf
}

fn utf8_entry(s: &str) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut e = Vec::new();
    e.push(1u8); // CONSTANT_Utf8
    e.write_u16::<BigEndian>(s.len() as u16).unwrap();
    e.extend_from_slice(s.as_bytes());
    e
}

fn class_entry(name_index: u16) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut e = Vec::new();
    e.push(7u8); // CONSTANT_Class
    e.write_u16::<BigEndian>(name_index).unwrap();
    e
}

fn int_entry(value: i32) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut e = Vec::new();
    e.push(3u8); // CONSTANT_Integer
    e.write_u32::<BigEndian>(value as u32).unwrap();
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn scan_resources_empty_dir() {
        let table = scan_resources(&PathBuf::from("/nonexistent"));
        assert!(table.entries.is_empty());
    }

    #[test]
    fn scan_resources_with_layouts() {
        let tmp = std::env::temp_dir().join("skotch-r-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let layout_dir = tmp.join("layout");
        std::fs::create_dir_all(&layout_dir).unwrap();
        std::fs::write(layout_dir.join("activity_main.xml"), "<LinearLayout/>").unwrap();
        std::fs::write(layout_dir.join("fragment_detail.xml"), "<FrameLayout/>").unwrap();

        let table = scan_resources(&tmp);
        let layouts = table.entries.get("layout").unwrap();
        assert_eq!(layouts.len(), 2);
        assert!(layouts.iter().any(|e| e.name == "activity_main"));
        assert!(layouts.iter().any(|e| e.name == "fragment_detail"));
        // IDs should be assigned
        assert_ne!(layouts[0].id, 0);
        assert_ne!(layouts[1].id, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scan_values_strings_xml() {
        let tmp = std::env::temp_dir().join("skotch-r-values-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let values_dir = tmp.join("values");
        std::fs::create_dir_all(&values_dir).unwrap();
        std::fs::write(
            values_dir.join("strings.xml"),
            r#"<?xml version="1.0"?>
<resources>
    <string name="app_name">MyApp</string>
    <string name="hello">Hello</string>
</resources>"#,
        )
        .unwrap();

        let table = scan_resources(&tmp);
        let strings = table.entries.get("string").unwrap();
        assert_eq!(strings.len(), 2);
        assert!(strings.iter().any(|e| e.name == "app_name"));
        assert!(strings.iter().any(|e| e.name == "hello"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_r_class_produces_valid_classfile() {
        let mut table = ResourceTable::default();
        table.entries.insert(
            "string".to_string(),
            vec![
                ResourceEntry {
                    name: "app_name".to_string(),
                    id: 0x7f090001,
                },
                ResourceEntry {
                    name: "hello".to_string(),
                    id: 0x7f090002,
                },
            ],
        );

        let classes = generate_r_class("com.example", &table);
        assert_eq!(classes.len(), 2); // R$string + R

        // Check the inner class has the CAFEBABE magic
        let (name, bytes) = &classes[0];
        assert_eq!(name, "com/example/R$string");
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);

        // Check the outer R class
        let (name, bytes) = &classes[1];
        assert_eq!(name, "com/example/R");
        assert_eq!(&bytes[0..4], &[0xCA, 0xFE, 0xBA, 0xBE]);
    }

    #[test]
    fn resource_ids_follow_android_convention() {
        let tmp = std::env::temp_dir().join("skotch-r-id-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let layout_dir = tmp.join("layout");
        std::fs::create_dir_all(&layout_dir).unwrap();
        std::fs::write(layout_dir.join("main.xml"), "").unwrap();
        let drawable_dir = tmp.join("drawable");
        std::fs::create_dir_all(&drawable_dir).unwrap();
        std::fs::write(drawable_dir.join("icon.png"), "").unwrap();

        let table = scan_resources(&tmp);
        let layout_id = table.entries["layout"][0].id;
        let drawable_id = table.entries["drawable"][0].id;

        // Package ID should be 0x7f
        assert_eq!(layout_id >> 24, 0x7f);
        assert_eq!(drawable_id >> 24, 0x7f);
        // Type IDs should differ
        assert_ne!((layout_id >> 16) & 0xff, (drawable_id >> 16) & 0xff);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
