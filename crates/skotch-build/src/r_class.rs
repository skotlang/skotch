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
    /// `int[] styleable X { id0, id1, ... }` arrays from R.txt, in
    /// declaration order — the index of each id within the array is
    /// what the `int styleable X_N` constants name. AAR R.txt files
    /// list these with 0x0 placeholder ids; the real ids come from
    /// aapt2's `--output-text-symbols` R.txt and are wired in after
    /// parsing by the caller via [`apply_styleable_arrays`].
    pub styleable_arrays: BTreeMap<String, Vec<u32>>,
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

/// Parse an AAR's `R.txt` file and build a `ResourceTable`. Each line has
/// the shape `<int|int[]> <type> <name> <hex_id>`, e.g.:
///   ```text
///   int drawable abc_action_bar_back_indicator 0x0
///   int string status_bar_notification_info_overflow 0x7f110100
///   ```
/// Library AARs ship `R.txt` with all `0x0` placeholder ids — the real
/// values get assigned at app-merge time by aapt2. We don't run aapt2's
/// resource link step, so we synthesize unique-within-library ids: type
/// id from `type_id_for` × 0x10000, entry id sequential within type, all
/// under package id 0x7f. That's enough for `Class.forName(...R$type)`
/// to find the field; runtime resource lookups still won't resolve to
/// the real bitmap/string, but the static init no longer NPEs which
/// unblocks Activity creation.
pub fn parse_r_txt(content: &str) -> ResourceTable {
    let mut table = ResourceTable::default();
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // `int[] styleable <name> { id, id, ... }` — array of resource
        // ids that AppCompat's TintTypedArray dereferences by index.
        if parts.len() >= 4 && parts[0] == "int[]" && parts[1] == "styleable" {
            let name = parts[2].to_string();
            // Collect ids between the braces; tolerate commas + 0x prefix.
            let after_brace = match line.find('{') {
                Some(i) => &line[i + 1..],
                None => continue,
            };
            let inside = after_brace.trim_end_matches('}').trim();
            let ids: Vec<u32> = inside
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .filter_map(|tok| {
                    let s = tok.trim().trim_start_matches("0x");
                    u32::from_str_radix(s, 16).ok()
                })
                .collect();
            table.styleable_arrays.insert(name, ids);
            continue;
        }
        // `int <type> <name> <value>` for non-styleable types; the value
        // is overwritten by `assign_resource_ids` since AAR R.txt files
        // ship with 0x0 placeholders.
        //
        // For `int styleable <name> <index>` rows, the value IS the
        // literal index into the array (a small integer) — preserve it
        // verbatim and skip `assign_resource_ids` below.
        if parts.len() < 4 || parts[0] != "int" {
            continue;
        }
        let res_type = parts[1].to_string();
        let name = parts[2].to_string();
        let id = if res_type == "styleable" {
            let raw = parts[3].trim_start_matches("0x");
            // styleable index values are usually decimal; allow hex too.
            raw.parse::<u32>()
                .ok()
                .or_else(|| u32::from_str_radix(raw, 16).ok())
                .unwrap_or(0)
        } else {
            0
        };
        table
            .entries
            .entry(res_type)
            .or_default()
            .push(ResourceEntry { name, id });
    }
    assign_resource_ids(&mut table);
    table
}

/// Replace each `int[]` styleable array's ids with values from `real`
/// (keyed by name). Caller obtains `real` by parsing aapt2's
/// `--output-text-symbols` R.txt, which lists each styleable array
/// with the resource ids actually baked into `resources.arsc`. Lookups
/// without a match leave the existing array intact so generation still
/// produces a structurally valid `R$styleable` class — runtime
/// dereferences for those will hit the placeholder 0x0 ids.
pub fn apply_styleable_arrays(
    table: &mut ResourceTable,
    real: &std::collections::HashMap<String, Vec<u32>>,
) {
    for (name, ids) in &mut table.styleable_arrays {
        if let Some(replacement) = real.get(name) {
            *ids = replacement.clone();
        }
    }
}

/// Re-derive each styleable's `int[]` array AND its per-attribute
/// integer indices from aapt2's link-time `symbol_ids` + `link_arrays`.
/// Library R.txt files ship pre-link indices ordered by the library's
/// own attribute declaration, but at app build time aapt2 sorts the
/// attribute IDs ascending in each `int[] styleable` array AND emits
/// the per-package R.java with indices that match the sorted ordering.
/// The AAR's pre-compiled bytecode reads `R$styleable.<S>_<attr>`
/// dynamically (no `ConstantValue` inlining at AAR build time), so it
/// picks up the runtime values from whatever `R$styleable.class` ends
/// up in the APK — those values must be aapt2's sorted ordering for
/// `TypedArray.getString(index)` to return the right attribute. We
/// keep the library's R.txt only as the source-of-truth for which
/// `(styleable, attr)` pairs this AAR owns.
pub fn apply_styleable_arrays_for_lib(
    table: &mut ResourceTable,
    symbol_ids: &std::collections::HashMap<(String, String), u32>,
    link_arrays: &std::collections::HashMap<String, Vec<u32>>,
) {
    let lib_styleable_entries: Vec<ResourceEntry> =
        table.entries.get("styleable").cloned().unwrap_or_default();
    let lib_arrays = table.styleable_arrays.clone();
    let array_names: Vec<String> = lib_arrays.keys().cloned().collect();
    let mut new_index_entries: Vec<ResourceEntry> = Vec::new();
    for styleable_name in &array_names {
        let prefix = format!("{styleable_name}_");
        let sorted_arr = match link_arrays.get(styleable_name) {
            Some(a) => a.clone(),
            None => continue,
        };
        let lib_arr = lib_arrays.get(styleable_name).cloned().unwrap_or_default();
        table
            .styleable_arrays
            .insert(styleable_name.clone(), sorted_arr.clone());
        for entry in &lib_styleable_entries {
            let Some(attr_name) = entry.name.strip_prefix(&prefix) else {
                continue;
            };
            let lib_pos = entry.id as usize;
            // Resolve the attribute's resource id. AAR R.txt arrays put
            // the framework attr id directly (e.g. `0x10100d0` for
            // `android:id`) and use `0x0` as a placeholder for
            // non-framework attrs that aapt2 fills in at link time.
            let lib_id = lib_arr.get(lib_pos).copied().unwrap_or(0);
            let resolved_id = if lib_id != 0 {
                Some(lib_id)
            } else {
                symbol_ids
                    .get(&("attr".to_string(), attr_name.to_string()))
                    .copied()
            };
            let Some(id) = resolved_id else {
                continue;
            };
            if let Some(pos) = sorted_arr.iter().position(|&v| v == id) {
                new_index_entries.push(ResourceEntry {
                    name: format!("{prefix}{attr_name}"),
                    id: pos as u32,
                });
            }
        }
    }
    if !new_index_entries.is_empty() {
        table
            .entries
            .insert("styleable".to_string(), new_index_entries);
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
/// Styleable index constants (`int styleable X_Y N`) preserve their
/// literal R.txt value — those are array indices, not resource ids,
/// so the synthetic `0x7f<type><idx>` form would be wrong.
fn assign_resource_ids(table: &mut ResourceTable) {
    const PACKAGE_ID: u32 = 0x7f;
    for (res_type, entries) in &mut table.entries {
        if res_type == "styleable" {
            continue;
        }
        let type_id = type_id_for(res_type) as u32;
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.id = (PACKAGE_ID << 24) | (type_id << 16) | (i as u32 + 1);
        }
    }
}

/// Generate a minimal `resources.arsc` binary table from the resource table.
///
/// The binary format follows Android's `ResourceTypes.h` with:
/// - ResTable_header (RES_TABLE_TYPE = 0x0002)
/// - Global string pool (ResStringPool with all resource values)
/// - Package chunk with type string pool, type specs, and type entries
///
/// This is a simplified implementation that handles string and integer
/// values. Complex resource types (styles, arrays) are deferred.
pub fn generate_resources_arsc(
    package_name: &str,
    table: &ResourceTable,
    values: &std::collections::HashMap<String, String>,
) -> Vec<u8> {
    use byteorder::{LittleEndian, WriteBytesExt};

    // ── 1. Build global string pool (resource values) ──────────────────
    let mut value_strings: Vec<String> = Vec::new();
    let mut value_indices: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for (res_type, entries) in &table.entries {
        for entry in entries {
            let key = format!("{}.{}", res_type, entry.name);
            if let Some(val) = values.get(&key) {
                if !value_indices.contains_key(val) {
                    let idx = value_strings.len() as u32;
                    value_indices.insert(val.clone(), idx);
                    value_strings.push(val.clone());
                }
            }
        }
    }

    // ── 2. Build type string pool (resource type names) ────────────────
    let type_names: Vec<String> = table.entries.keys().cloned().collect();

    // ── 3. Build key string pool (resource entry names) ────────────────
    let mut key_strings: Vec<String> = Vec::new();
    for entries in table.entries.values() {
        for entry in entries {
            key_strings.push(entry.name.clone());
        }
    }

    // ── 4. Encode string pools ─────────────────────────────────────────
    let global_pool = encode_string_pool(&value_strings);
    let type_pool = encode_string_pool(&type_names);
    let key_pool = encode_string_pool(&key_strings);

    // ── 5. Build package chunk ─────────────────────────────────────────
    let mut package_data = Vec::new();

    // Package ID
    package_data.write_u32::<LittleEndian>(0x7f).unwrap();

    // Package name (128 UTF-16LE code units, zero-padded)
    let mut pkg_name_buf = vec![0u8; 256];
    for (i, ch) in package_name.encode_utf16().enumerate() {
        if i >= 127 {
            break;
        }
        pkg_name_buf[i * 2] = (ch & 0xFF) as u8;
        pkg_name_buf[i * 2 + 1] = (ch >> 8) as u8;
    }
    package_data.extend_from_slice(&pkg_name_buf);

    // typeStrings offset (from start of package chunk header)
    let type_strings_offset = 4 + 256 + 4 + 4 + 4 + 4; // after fixed header fields
    package_data
        .write_u32::<LittleEndian>(type_strings_offset as u32)
        .unwrap();
    // lastPublicType
    package_data
        .write_u32::<LittleEndian>(type_names.len() as u32)
        .unwrap();
    // keyStrings offset
    let key_strings_offset = type_strings_offset + type_pool.len();
    package_data
        .write_u32::<LittleEndian>(key_strings_offset as u32)
        .unwrap();
    // lastPublicKey
    package_data
        .write_u32::<LittleEndian>(key_strings.len() as u32)
        .unwrap();

    // Type string pool
    package_data.extend_from_slice(&type_pool);
    // Key string pool
    package_data.extend_from_slice(&key_pool);

    // Type specs and type entries (simplified: just mark all as public)
    for (type_idx, (_, entries)) in table.entries.iter().enumerate() {
        // ResTable_typeSpec
        let mut spec = Vec::new();
        spec.write_u16::<LittleEndian>(0x0202).unwrap(); // RES_TABLE_TYPE_SPEC_TYPE
        spec.write_u16::<LittleEndian>(8).unwrap(); // header size
        spec.write_u32::<LittleEndian>((8 + entries.len() * 4) as u32)
            .unwrap(); // chunk size
        spec.push((type_idx + 1) as u8); // type ID (1-based)
        spec.push(0); // res0
        spec.write_u16::<LittleEndian>(0).unwrap(); // res1
        spec.write_u32::<LittleEndian>(entries.len() as u32)
            .unwrap(); // entryCount
        for _ in entries {
            spec.write_u32::<LittleEndian>(0).unwrap(); // flags (0 = no config variation)
        }
        package_data.extend_from_slice(&spec);
    }

    // Wrap package chunk with header
    let package_header_size = 288u16; // fixed package header size
    let package_chunk_size = (package_header_size as usize) + package_data.len();
    let mut package_chunk = Vec::new();
    package_chunk.write_u16::<LittleEndian>(0x0200).unwrap(); // RES_TABLE_PACKAGE_TYPE
    package_chunk
        .write_u16::<LittleEndian>(package_header_size)
        .unwrap();
    package_chunk
        .write_u32::<LittleEndian>(package_chunk_size as u32)
        .unwrap();
    package_chunk.extend_from_slice(&package_data);

    // ── 6. Build the complete ResTable ─────────────────────────────────
    let mut arsc = Vec::new();
    let table_header_size = 12u16;
    let total_size = table_header_size as usize + global_pool.len() + package_chunk.len();
    arsc.write_u16::<LittleEndian>(0x0002).unwrap(); // RES_TABLE_TYPE
    arsc.write_u16::<LittleEndian>(table_header_size).unwrap();
    arsc.write_u32::<LittleEndian>(total_size as u32).unwrap();
    arsc.write_u32::<LittleEndian>(1).unwrap(); // packageCount
    arsc.extend_from_slice(&global_pool);
    arsc.extend_from_slice(&package_chunk);

    arsc
}

/// Encode a list of strings as an Android string pool chunk.
fn encode_string_pool(strings: &[String]) -> Vec<u8> {
    use byteorder::{LittleEndian, WriteBytesExt};

    let header_size = 28u16;
    let string_count = strings.len() as u32;

    // Encode strings as UTF-8 with length prefix.
    let mut string_data = Vec::new();
    let mut offsets = Vec::new();
    for s in strings {
        offsets.push(string_data.len() as u32);
        let bytes = s.as_bytes();
        // UTF-8 length (varint: len as u8 if < 128)
        string_data.push(bytes.len() as u8);
        // UTF-8 length again (Android quirk)
        string_data.push(bytes.len() as u8);
        string_data.extend_from_slice(bytes);
        string_data.push(0); // null terminator
    }

    let offsets_size = string_count as usize * 4;
    let strings_start = header_size as usize + offsets_size;
    // Pad string data to 4-byte boundary (Android requirement).
    while string_data.len() % 4 != 0 {
        string_data.push(0);
    }
    let chunk_size = strings_start + string_data.len();

    let mut pool = Vec::new();
    pool.write_u16::<LittleEndian>(0x0001).unwrap(); // RES_STRING_POOL_TYPE
    pool.write_u16::<LittleEndian>(header_size).unwrap();
    pool.write_u32::<LittleEndian>(chunk_size as u32).unwrap();
    pool.write_u32::<LittleEndian>(string_count).unwrap(); // stringCount
    pool.write_u32::<LittleEndian>(0).unwrap(); // styleCount
    pool.write_u32::<LittleEndian>(0x0100).unwrap(); // flags: UTF-8
    pool.write_u32::<LittleEndian>(strings_start as u32)
        .unwrap(); // stringsStart
    pool.write_u32::<LittleEndian>(0).unwrap(); // stylesStart

    // String offsets
    for offset in &offsets {
        pool.write_u32::<LittleEndian>(*offset).unwrap();
    }
    // String data
    pool.extend_from_slice(&string_data);

    pool
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
    // R$styleable also gets the `int[]` array fields declared by
    // `int[] styleable X { ... }` lines, initialized in <clinit>.
    let empty_entries: Vec<ResourceEntry> = Vec::new();
    let mut styleable_emitted = false;
    for (res_type, entries) in &table.entries {
        let inner_name = format!("{}${}", r_class_name, res_type);
        if res_type == "styleable" {
            let class_bytes =
                generate_styleable_r_class(&inner_name, entries, &table.styleable_arrays);
            classes.push((inner_name, class_bytes));
            styleable_emitted = true;
        } else {
            let class_bytes = generate_inner_r_class(&inner_name, entries);
            classes.push((inner_name, class_bytes));
        }
    }
    // If the AAR declares styleable arrays but no `int styleable` index
    // constants (rare but legal), still emit a R$styleable class so the
    // array fields are reachable.
    if !styleable_emitted && !table.styleable_arrays.is_empty() {
        let inner_name = format!("{}$styleable", r_class_name);
        let class_bytes =
            generate_styleable_r_class(&inner_name, &empty_entries, &table.styleable_arrays);
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

/// Generate bytecode for `R$styleable`: a class with int constants AND
/// `int[]` static array fields initialized by a `<clinit>` method.
///
/// AppCompat reads `R$styleable.SomeWidget[N]` where `N` is one of
/// the `int styleable SomeWidget_<attr>` index constants. The array's
/// elements are resource ids that aapt2 baked into `resources.arsc`.
fn generate_styleable_r_class(
    class_name: &str,
    entries: &[ResourceEntry],
    arrays: &BTreeMap<String, Vec<u32>>,
) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut buf = Vec::new();

    buf.write_u32::<BigEndian>(0xCAFEBABE).unwrap();
    buf.write_u16::<BigEndian>(0).unwrap();
    buf.write_u16::<BigEndian>(61).unwrap();

    // ── Constant pool ──
    // Indices 1..6 are the standard "this class + super + descriptors"
    // boilerplate. After that we append, in order:
    //   * per-int-field: 1 Utf8 (name) + 1 Integer (constant value)
    //   * per-array-field: 1 Utf8 (name)
    //   * Code-emission constants: each unique array length and each
    //     unique array element id may need a CONSTANT_Integer if it
    //     can't be encoded inline with iconst_*/bipush/sipush. We
    //     synthesize those on demand below.
    let mut cp: Vec<Vec<u8>> = vec![
        Vec::new(),                     // index 0 unused
        utf8_entry(class_name),         // #1
        class_entry(1),                 // #2: this
        utf8_entry("java/lang/Object"), // #3
        class_entry(3),                 // #4: super
        utf8_entry("I"),                // #5: int descriptor
        utf8_entry("[I"),               // #6: int[] descriptor
        utf8_entry("ConstantValue"),    // #7: attr name
        utf8_entry("Code"),             // #8: attr name
        utf8_entry("<clinit>"),         // #9: method name
        utf8_entry("()V"),              // #10: method desc
    ];

    // Field metadata: (name_idx, kind) where kind = Some(const_idx) for
    // int fields with a ConstantValue attribute, or None for int[]
    // fields initialized in <clinit>.
    let mut int_fields: Vec<(u16, u16)> = Vec::new(); // (name_idx, const_value_idx)
    for entry in entries {
        let name_idx = cp.len() as u16;
        cp.push(utf8_entry(&entry.name));
        let val_idx = cp.len() as u16;
        cp.push(int_entry(entry.id as i32));
        int_fields.push((name_idx, val_idx));
    }
    let mut array_fields: Vec<(u16, Vec<u32>, u16)> = Vec::new();
    // (name_idx, ids, fieldref_idx). fieldref_idx points to a
    // CONSTANT_Fieldref so <clinit>'s putstatic can resolve it.
    for (name, ids) in arrays {
        let name_idx = cp.len() as u16;
        cp.push(utf8_entry(name));
        // NameAndType for putstatic: (name_idx, [I_desc_idx=6).
        let nat_idx = cp.len() as u16;
        cp.push(name_and_type_entry(name_idx, 6));
        let fieldref_idx = cp.len() as u16;
        cp.push(fieldref_entry(2, nat_idx)); // class=#2 (this)
        array_fields.push((name_idx, ids.clone(), fieldref_idx));
    }

    // Pre-allocate Integer CP entries for any id that can't be encoded
    // inline (i.e. > 32767 or < -32768). Most resource ids exceed
    // 0x7f0000 so they always need a CP entry.
    let mut id_const_cp: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
    for (_, ids, _) in &array_fields {
        for &id in ids {
            if !fits_in_sipush(id) && !id_const_cp.contains_key(&id) {
                let idx = cp.len() as u16;
                cp.push(int_entry(id as i32));
                id_const_cp.insert(id, idx);
            }
        }
    }

    let cp_count = cp.len() as u16;
    buf.write_u16::<BigEndian>(cp_count).unwrap();
    for entry in &cp[1..] {
        buf.extend_from_slice(entry);
    }

    // Access flags: ACC_PUBLIC | ACC_FINAL | ACC_SUPER
    buf.write_u16::<BigEndian>(0x0031).unwrap();
    buf.write_u16::<BigEndian>(2).unwrap(); // this
    buf.write_u16::<BigEndian>(4).unwrap(); // super
    buf.write_u16::<BigEndian>(0).unwrap(); // interfaces

    // ── Fields ──
    let total_fields = int_fields.len() + array_fields.len();
    buf.write_u16::<BigEndian>(total_fields as u16).unwrap();
    for (name_idx, const_idx) in &int_fields {
        buf.write_u16::<BigEndian>(0x0019).unwrap(); // public static final
        buf.write_u16::<BigEndian>(*name_idx).unwrap();
        buf.write_u16::<BigEndian>(5).unwrap(); // "I"
        buf.write_u16::<BigEndian>(1).unwrap(); // 1 attribute
        buf.write_u16::<BigEndian>(7).unwrap(); // ConstantValue
        buf.write_u32::<BigEndian>(2).unwrap();
        buf.write_u16::<BigEndian>(*const_idx).unwrap();
    }
    for (name_idx, _, _) in &array_fields {
        buf.write_u16::<BigEndian>(0x0019).unwrap(); // public static final
        buf.write_u16::<BigEndian>(*name_idx).unwrap();
        buf.write_u16::<BigEndian>(6).unwrap(); // "[I"
        buf.write_u16::<BigEndian>(0).unwrap(); // 0 attributes (init in <clinit>)
    }

    // ── Methods ──
    // Emit a <clinit>()V if there are any array fields to initialize.
    if array_fields.is_empty() {
        buf.write_u16::<BigEndian>(0).unwrap();
    } else {
        buf.write_u16::<BigEndian>(1).unwrap(); // 1 method
                                                // method_info: access (ACC_STATIC=0x0008), name (#9), desc (#10),
                                                // attributes=1 (Code).
        buf.write_u16::<BigEndian>(0x0008).unwrap();
        buf.write_u16::<BigEndian>(9).unwrap();
        buf.write_u16::<BigEndian>(10).unwrap();
        buf.write_u16::<BigEndian>(1).unwrap();
        // Code attribute.
        let code = build_styleable_clinit(&array_fields, &id_const_cp);
        let max_stack: u16 = 4; // worst case: arrayref, arrayref, index, value
        let max_locals: u16 = 0;
        buf.write_u16::<BigEndian>(8).unwrap(); // attribute_name_index = "Code"
                                                // attribute_length = 2 + 2 + 4 + code.len() + 2 + 2
        let attr_len = 12 + code.len() as u32;
        buf.write_u32::<BigEndian>(attr_len).unwrap();
        buf.write_u16::<BigEndian>(max_stack).unwrap();
        buf.write_u16::<BigEndian>(max_locals).unwrap();
        buf.write_u32::<BigEndian>(code.len() as u32).unwrap();
        buf.extend_from_slice(&code);
        buf.write_u16::<BigEndian>(0).unwrap(); // exception_table_length
        buf.write_u16::<BigEndian>(0).unwrap(); // attributes_count
    }

    // ── Class attributes (none) ──
    buf.write_u16::<BigEndian>(0).unwrap();
    buf
}

fn fits_in_sipush(id: u32) -> bool {
    let signed = id as i32;
    (-32768..=32767).contains(&signed)
}

fn build_styleable_clinit(
    array_fields: &[(u16, Vec<u32>, u16)],
    id_const_cp: &std::collections::HashMap<u32, u16>,
) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut code = Vec::new();
    for (_, ids, fieldref_idx) in array_fields {
        push_int_const(&mut code, ids.len() as i32);
        // newarray T_INT (atype=10)
        code.push(0xBC);
        code.push(10);
        for (i, &id) in ids.iter().enumerate() {
            // dup; iconst i (or bipush); ldc id (or inline); iastore
            code.push(0x59); // dup
            push_int_const(&mut code, i as i32);
            if let Some(&cp_idx) = id_const_cp.get(&id) {
                if cp_idx < 256 {
                    code.push(0x12); // ldc
                    code.push(cp_idx as u8);
                } else {
                    code.push(0x13); // ldc_w
                    code.write_u16::<BigEndian>(cp_idx).unwrap();
                }
            } else {
                push_int_const(&mut code, id as i32);
            }
            code.push(0x4F); // iastore
        }
        code.push(0xB3); // putstatic
        code.write_u16::<BigEndian>(*fieldref_idx).unwrap();
    }
    code.push(0xB1); // return
    code
}

/// Push an `int` constant using the smallest valid encoding
/// (iconst_*, bipush, sipush). Callers must use ldc for values outside
/// the sipush range (handled separately for resource ids).
fn push_int_const(code: &mut Vec<u8>, v: i32) {
    use byteorder::{BigEndian, WriteBytesExt};
    if (-1..=5).contains(&v) {
        // iconst_m1 = 0x02, iconst_0 = 0x03, ..., iconst_5 = 0x08.
        code.push((0x03_i32 + v) as u8);
    } else if (-128..=127).contains(&v) {
        code.push(0x10); // bipush
        code.push(v as u8);
    } else if (-32768..=32767).contains(&v) {
        code.push(0x11); // sipush
        code.write_i16::<BigEndian>(v as i16).unwrap();
    } else {
        // Caller should have routed through ldc; emit iconst_0 as a
        // structural fallback so the verifier still accepts the class.
        code.push(0x03);
    }
}

fn name_and_type_entry(name_idx: u16, desc_idx: u16) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut e = Vec::new();
    e.push(12u8); // CONSTANT_NameAndType
    e.write_u16::<BigEndian>(name_idx).unwrap();
    e.write_u16::<BigEndian>(desc_idx).unwrap();
    e
}

fn fieldref_entry(class_idx: u16, nat_idx: u16) -> Vec<u8> {
    use byteorder::{BigEndian, WriteBytesExt};
    let mut e = Vec::new();
    e.push(9u8); // CONSTANT_Fieldref
    e.write_u16::<BigEndian>(class_idx).unwrap();
    e.write_u16::<BigEndian>(nat_idx).unwrap();
    e
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

    #[test]
    fn generate_resources_arsc_basic() {
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
        let mut values = std::collections::HashMap::new();
        values.insert("string.app_name".to_string(), "MyApp".to_string());
        values.insert("string.hello".to_string(), "Hello World".to_string());

        let arsc = generate_resources_arsc("com.example", &table, &values);
        // Verify the ARSC starts with the correct type header
        assert!(arsc.len() > 12);
        assert_eq!(arsc[0], 0x02); // RES_TABLE_TYPE low byte
        assert_eq!(arsc[1], 0x00); // RES_TABLE_TYPE high byte
    }

    #[test]
    fn encode_string_pool_roundtrip() {
        let strings = vec!["hello".to_string(), "world".to_string()];
        let pool = super::encode_string_pool(&strings);
        // Verify string pool header
        assert_eq!(pool[0], 0x01); // RES_STRING_POOL_TYPE low
        assert_eq!(pool[1], 0x00);
        // String count should be 2
        assert_eq!(pool[8], 2);
        assert_eq!(pool[9], 0);
    }
}
