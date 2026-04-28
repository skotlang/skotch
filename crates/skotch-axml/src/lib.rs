//! Android binary XML (AXML) encoder.
//!
//! Encodes an XML element tree into the binary format that Android's
//! runtime reads from `AndroidManifest.xml` inside an APK. This is the
//! same format that `aapt2 compile` produces.
//!
//! ## Format overview
//!
//! The file is a sequence of **chunks**, each with a `(type: u16,
//! header_size: u16, size: u32)` header. Chunk types:
//!
//! - `0x0003` — XML resource file (outermost wrapper)
//! - `0x0001` — String pool
//! - `0x0180` — Resource ID table (maps string indices → android R.attr IDs)
//! - `0x0100` — XML namespace start
//! - `0x0101` — XML namespace end
//! - `0x0102` — XML element start
//! - `0x0103` — XML element end

use byteorder::{LittleEndian, WriteBytesExt};
use std::collections::HashMap;
use std::io::Write;

// ── Public types ────────────────────────────────────────────────────────

/// An XML element with optional namespace, attributes, and children.
#[derive(Clone, Debug)]
pub struct Element {
    pub namespace: Option<String>,
    pub name: String,
    pub attributes: Vec<Attribute>,
    pub children: Vec<Element>,
}

/// An XML attribute.
#[derive(Clone, Debug)]
pub struct Attribute {
    pub namespace: Option<String>,
    pub name: String,
    /// Android resource ID for well-known attributes (e.g. `0x0101021B`
    /// for `android:versionCode`). `None` for non-android attributes.
    pub resource_id: Option<u32>,
    pub value: AttributeValue,
}

/// Typed attribute value.
#[derive(Clone, Debug)]
pub enum AttributeValue {
    String(String),
    Integer(i32),
    Boolean(bool),
    Reference(u32),
}

// ── Well-known Android attribute resource IDs ───────────────────────────

/// Map of well-known Android attribute names to their resource IDs.
fn android_attr_ids() -> HashMap<&'static str, u32> {
    let mut m = HashMap::new();
    m.insert("versionCode", 0x0101_021B);
    m.insert("versionName", 0x0101_021C);
    m.insert("minSdkVersion", 0x0101_020C);
    m.insert("targetSdkVersion", 0x0101_0270);
    m.insert("name", 0x0101_0003);
    m.insert("label", 0x0101_0001);
    m.insert("icon", 0x0101_0002);
    m.insert("theme", 0x0101_000E);
    m.insert("configChanges", 0x0101_0153);
    m.insert("exported", 0x0101_0010);
    m.insert("enabled", 0x0101_0000);
    m.insert("permission", 0x0101_0006);
    m.insert("hardwareAccelerated", 0x0101_0347);
    m.insert("allowBackup", 0x0101_0280);
    m.insert("supportsRtl", 0x0101_0382);
    m.insert("debuggable", 0x0101_000F);
    m.insert("roundIcon", 0x0101_048C);
    m.insert("windowSoftInputMode", 0x0101_022B);
    m.insert("resource", 0x0101_0025);
    m.insert("value", 0x0101_0024);
    m.insert("enableOnBackInvokedCallback", 0x0101_0627);
    m
}

const ANDROID_NS: &str = "http://schemas.android.com/apk/res/android";

// ── Public API ──────────────────────────────────────────────────────────

/// Parse a source AndroidManifest.xml string into an Element tree.
///
/// This is a minimal XML parser that handles the subset of XML used in
/// Android manifest files: elements, attributes with `android:` namespace,
/// text content (ignored), and comments.
pub fn parse_source_manifest(xml: &str) -> Option<Element> {
    let ids = android_attr_ids();
    parse_element(&mut xml.trim(), &ids)
}

fn parse_element(src: &mut &str, ids: &HashMap<&str, u32>) -> Option<Element> {
    // Skip whitespace, comments, XML declaration
    loop {
        *src = src.trim_start();
        if src.starts_with("<?") {
            if let Some(end) = src.find("?>") {
                *src = &src[end + 2..];
                continue;
            }
        }
        if src.starts_with("<!--") {
            if let Some(end) = src.find("-->") {
                *src = &src[end + 3..];
                continue;
            }
        }
        break;
    }

    *src = src.trim_start();
    if !src.starts_with('<') || src.starts_with("</") {
        return None;
    }

    // Parse opening tag
    *src = &src[1..]; // consume '<'
    let tag_end = src.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
    let tag_name = src[..tag_end].to_string();
    *src = &src[tag_end..];

    // Parse attributes
    let mut attributes = Vec::new();
    loop {
        *src = src.trim_start();
        if src.starts_with('>') || src.starts_with("/>") {
            break;
        }
        // Parse attr_name="value"
        let eq_pos = src.find('=')?;
        let attr_full = src[..eq_pos].trim();
        *src = &src[eq_pos + 1..];
        *src = src.trim_start();

        // Parse value (quoted string)
        let quote = src.chars().next()?;
        if quote != '"' && quote != '\'' {
            break;
        }
        *src = &src[1..];
        let val_end = src.find(quote)?;
        let value_str = src[..val_end].to_string();
        *src = &src[val_end + 1..];

        // Split namespace:name
        let (ns, attr_name) = if let Some(colon) = attr_full.find(':') {
            let prefix = &attr_full[..colon];
            let name = &attr_full[colon + 1..];
            if prefix == "android" {
                (Some(ANDROID_NS.to_string()), name.to_string())
            } else if prefix == "xmlns" {
                // Skip xmlns declarations — they're metadata.
                continue;
            } else {
                (None, attr_full.to_string())
            }
        } else {
            (None, attr_full.to_string())
        };

        // Determine typed value — resolve Android enum/flag constants.
        let value = if value_str.starts_with("@") {
            // Resource references like @mipmap/ic_launcher need resolved R IDs.
            // Since we don't have a fully compiled R class, strip resource
            // references by using a placeholder string. This allows the manifest
            // to be installed without a valid resources.arsc.
            AttributeValue::String(
                value_str
                    .strip_prefix("@string/")
                    .unwrap_or(&value_str)
                    .to_string(),
            )
        } else if value_str == "true" {
            AttributeValue::Boolean(true)
        } else if value_str == "false" {
            AttributeValue::Boolean(false)
        } else if let Ok(n) = value_str.parse::<i32>() {
            AttributeValue::Integer(n)
        } else if let Some(int_val) = resolve_android_enum(&attr_name, &value_str) {
            AttributeValue::Integer(int_val)
        } else if value_str.starts_with("0x") {
            value_str
                .strip_prefix("0x")
                .and_then(|h| i32::from_str_radix(h, 16).ok())
                .map(AttributeValue::Integer)
                .unwrap_or(AttributeValue::String(value_str))
        } else {
            AttributeValue::String(value_str)
        };

        let resource_id = if ns.is_some() {
            ids.get(attr_name.as_str()).copied()
        } else {
            None
        };

        attributes.push(Attribute {
            namespace: ns,
            name: attr_name,
            resource_id,
            value,
        });
    }

    // Check for self-closing tag
    if src.starts_with("/>") {
        *src = &src[2..];
        return Some(Element {
            namespace: None,
            name: tag_name,
            attributes,
            children: Vec::new(),
        });
    }

    // Consume '>'
    if src.starts_with('>') {
        *src = &src[1..];
    }

    // Parse children
    let mut children = Vec::new();
    loop {
        *src = src.trim_start();
        if src.is_empty() {
            break;
        }
        // Check for closing tag
        let close_tag = format!("</{tag_name}");
        if src.starts_with(&close_tag) {
            *src = &src[close_tag.len()..];
            // Skip past '>'
            if let Some(gt) = src.find('>') {
                *src = &src[gt + 1..];
            }
            break;
        }
        // Try to parse child element
        if src.starts_with('<') && !src.starts_with("</") {
            if let Some(child) = parse_element(src, ids) {
                children.push(child);
            } else {
                // Skip unknown content
                if let Some(gt) = src.find('>') {
                    *src = &src[gt + 1..];
                } else {
                    break;
                }
            }
        } else {
            // Skip text content
            if let Some(lt) = src.find('<') {
                *src = &src[lt..];
            } else {
                break;
            }
        }
    }

    // Filter out elements that reference unresolved resources
    // (like <meta-data android:resource="@xml/...">) to prevent
    // install failures. Keep only essential elements.
    let children = children
        .into_iter()
        .filter(|child| {
            // Keep meta-data only if it doesn't have unresolved @resource refs.
            if child.name == "meta-data" {
                return child.attributes.iter().all(|a| {
                    if a.name == "resource" {
                        !matches!(&a.value, AttributeValue::String(s) if s.starts_with('@'))
                    } else {
                        true
                    }
                });
            }
            true
        })
        .collect();

    Some(Element {
        namespace: None,
        name: tag_name,
        attributes,
        children,
    })
}

/// Resolve Android attribute enum/flag values to their integer constants.
/// E.g. `windowSoftInputMode="adjustResize"` → 0x10.
fn resolve_android_enum(attr_name: &str, value: &str) -> Option<i32> {
    match (attr_name, value) {
        // windowSoftInputMode flags
        ("windowSoftInputMode", "adjustNothing") => Some(0x30),
        ("windowSoftInputMode", "adjustResize") => Some(0x10),
        ("windowSoftInputMode", "adjustPan") => Some(0x20),
        ("windowSoftInputMode", "adjustUnspecified") => Some(0x00),
        ("windowSoftInputMode", "stateUnspecified") => Some(0x00),
        ("windowSoftInputMode", "stateHidden") => Some(0x02),
        ("windowSoftInputMode", "stateVisible") => Some(0x04),
        ("windowSoftInputMode", "stateAlwaysHidden") => Some(0x03),
        ("windowSoftInputMode", "stateAlwaysVisible") => Some(0x05),
        // configChanges flags
        ("configChanges", "orientation") => Some(0x0080),
        ("configChanges", "screenSize") => Some(0x0400),
        ("configChanges", "keyboardHidden") => Some(0x0020),
        // launchMode
        ("launchMode", "standard") => Some(0),
        ("launchMode", "singleTop") => Some(1),
        ("launchMode", "singleTask") => Some(2),
        ("launchMode", "singleInstance") => Some(3),
        // screenOrientation
        ("screenOrientation", "unspecified") => Some(-1),
        ("screenOrientation", "portrait") => Some(1),
        ("screenOrientation", "landscape") => Some(0),
        _ => None,
    }
}

/// Encode an XML element tree into Android binary XML bytes.
pub fn encode_axml(root: &Element) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.encode(root)
}

/// Build a minimal `AndroidManifest.xml` element tree suitable for a
/// simple Android application.
pub fn build_manifest(
    package: &str,
    version_code: u32,
    version_name: &str,
    min_sdk: u32,
    target_sdk: u32,
    main_activity: Option<&str>,
) -> Element {
    let ids = android_attr_ids();

    let uses_sdk = Element {
        namespace: None,
        name: "uses-sdk".into(),
        attributes: vec![
            Attribute {
                namespace: Some(ANDROID_NS.into()),
                name: "minSdkVersion".into(),
                resource_id: ids.get("minSdkVersion").copied(),
                value: AttributeValue::Integer(min_sdk as i32),
            },
            Attribute {
                namespace: Some(ANDROID_NS.into()),
                name: "targetSdkVersion".into(),
                resource_id: ids.get("targetSdkVersion").copied(),
                value: AttributeValue::Integer(target_sdk as i32),
            },
        ],
        children: vec![],
    };

    let mut app_children = Vec::new();
    if let Some(activity_name) = main_activity {
        let intent_filter = Element {
            namespace: None,
            name: "intent-filter".into(),
            attributes: vec![],
            children: vec![
                Element {
                    namespace: None,
                    name: "action".into(),
                    attributes: vec![Attribute {
                        namespace: Some(ANDROID_NS.into()),
                        name: "name".into(),
                        resource_id: ids.get("name").copied(),
                        value: AttributeValue::String("android.intent.action.MAIN".into()),
                    }],
                    children: vec![],
                },
                Element {
                    namespace: None,
                    name: "category".into(),
                    attributes: vec![Attribute {
                        namespace: Some(ANDROID_NS.into()),
                        name: "name".into(),
                        resource_id: ids.get("name").copied(),
                        value: AttributeValue::String("android.intent.category.LAUNCHER".into()),
                    }],
                    children: vec![],
                },
            ],
        };
        app_children.push(Element {
            namespace: None,
            name: "activity".into(),
            attributes: vec![Attribute {
                namespace: Some(ANDROID_NS.into()),
                name: "name".into(),
                resource_id: ids.get("name").copied(),
                value: AttributeValue::String(activity_name.into()),
            }],
            children: vec![intent_filter],
        });
    }

    let application = Element {
        namespace: None,
        name: "application".into(),
        attributes: vec![Attribute {
            namespace: Some(ANDROID_NS.into()),
            name: "label".into(),
            resource_id: ids.get("label").copied(),
            value: AttributeValue::String(package.rsplit('.').next().unwrap_or(package).into()),
        }],
        children: app_children,
    };

    Element {
        namespace: None,
        name: "manifest".into(),
        attributes: vec![
            Attribute {
                namespace: None,
                name: "package".into(),
                resource_id: None,
                value: AttributeValue::String(package.into()),
            },
            Attribute {
                namespace: Some(ANDROID_NS.into()),
                name: "versionCode".into(),
                resource_id: ids.get("versionCode").copied(),
                value: AttributeValue::Integer(version_code as i32),
            },
            Attribute {
                namespace: Some(ANDROID_NS.into()),
                name: "versionName".into(),
                resource_id: ids.get("versionName").copied(),
                value: AttributeValue::String(version_name.into()),
            },
        ],
        children: vec![uses_sdk, application],
    }
}

// ── Encoder internals ───────────────────────────────────────────────────

struct Encoder {
    /// String pool: index → string. Attribute-name strings with resource
    /// IDs must come first (in resource-ID-table order).
    strings: Vec<String>,
    string_map: HashMap<String, u32>,
    /// Resource IDs for attribute-name strings, in the same order as
    /// the first N entries in the string pool.
    resource_ids: Vec<u32>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            strings: Vec::new(),
            string_map: HashMap::new(),
            resource_ids: Vec::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&idx) = self.string_map.get(s) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.string_map.insert(s.to_string(), idx);
        self.strings.push(s.to_string());
        idx
    }

    fn encode(&mut self, root: &Element) -> Vec<u8> {
        // Phase 1: collect all attribute-name strings that have resource IDs.
        // These MUST be at the start of the string pool.
        self.collect_resource_attrs(root);

        // Phase 2: intern all remaining strings (ns URIs, element names,
        // attribute names without resource IDs, string values).
        self.intern(ANDROID_NS);
        self.intern("android");
        self.intern_element_strings(root);

        // Phase 3: encode chunks.
        let string_pool = self.encode_string_pool();
        let res_ids = self.encode_resource_ids();
        let body = self.encode_element(root);

        // Namespace start/end.
        let ns_prefix_idx = self.string_map["android"];
        let ns_uri_idx = self.string_map[ANDROID_NS];
        let ns_start = encode_namespace(0x0100, ns_prefix_idx, ns_uri_idx);
        let ns_end = encode_namespace(0x0101, ns_prefix_idx, ns_uri_idx);

        // Wrap in file header.
        let inner_size =
            string_pool.len() + res_ids.len() + ns_start.len() + body.len() + ns_end.len();
        let total_size = 8 + inner_size; // 8 = file header

        let mut out = Vec::with_capacity(total_size);
        out.write_u16::<LittleEndian>(0x0003).unwrap(); // type: XML
        out.write_u16::<LittleEndian>(8).unwrap(); // header size
        out.write_u32::<LittleEndian>(total_size as u32).unwrap();
        out.write_all(&string_pool).unwrap();
        out.write_all(&res_ids).unwrap();
        out.write_all(&ns_start).unwrap();
        out.write_all(&body).unwrap();
        out.write_all(&ns_end).unwrap();
        out
    }

    /// Collect attribute names with resource IDs into the string pool
    /// first, so their indices match the resource ID table.
    fn collect_resource_attrs(&mut self, elem: &Element) {
        for attr in &elem.attributes {
            if let Some(rid) = attr.resource_id {
                if !self.string_map.contains_key(&attr.name) {
                    let idx = self.strings.len() as u32;
                    self.string_map.insert(attr.name.clone(), idx);
                    self.strings.push(attr.name.clone());
                    self.resource_ids.push(rid);
                }
            }
        }
        for child in &elem.children {
            self.collect_resource_attrs(child);
        }
    }

    fn intern_element_strings(&mut self, elem: &Element) {
        if let Some(ns) = &elem.namespace {
            self.intern(ns);
        }
        self.intern(&elem.name);
        for attr in &elem.attributes {
            if let Some(ns) = &attr.namespace {
                self.intern(ns);
            }
            self.intern(&attr.name);
            if let AttributeValue::String(s) = &attr.value {
                self.intern(s);
            }
        }
        for child in &elem.children {
            self.intern_element_strings(child);
        }
    }

    fn encode_string_pool(&self) -> Vec<u8> {
        let count = self.strings.len() as u32;
        // Encode each string as: u16 char_len, u16 byte_len, UTF-8 bytes, 0x00.
        let mut string_data = Vec::new();
        let mut offsets = Vec::new();
        for s in &self.strings {
            offsets.push(string_data.len() as u32);
            let chars = s.encode_utf16().count();
            let bytes = s.len();
            // UTF-8 flag encoding: length as u8 pairs.
            string_data.push(chars as u8);
            if chars > 0x7F {
                // High bit set for two-byte length. Simplified: we only
                // support strings < 128 chars for now.
                string_data.push(0);
            }
            string_data.push(bytes as u8);
            if bytes > 0x7F {
                string_data.push(0);
            }
            string_data.extend_from_slice(s.as_bytes());
            string_data.push(0); // null terminator
        }
        // Pad string data to 4-byte boundary.
        while string_data.len() % 4 != 0 {
            string_data.push(0);
        }

        let header_size: u16 = 28;
        let offsets_size = count * 4;
        let chunk_size = header_size as u32 + offsets_size + string_data.len() as u32;
        let strings_start = header_size as u32 + offsets_size;

        let mut out = Vec::new();
        out.write_u16::<LittleEndian>(0x0001).unwrap(); // type
        out.write_u16::<LittleEndian>(header_size).unwrap();
        out.write_u32::<LittleEndian>(chunk_size).unwrap();
        out.write_u32::<LittleEndian>(count).unwrap(); // string count
        out.write_u32::<LittleEndian>(0).unwrap(); // style count
        out.write_u32::<LittleEndian>(0x100).unwrap(); // flags: UTF-8
        out.write_u32::<LittleEndian>(strings_start).unwrap();
        out.write_u32::<LittleEndian>(0).unwrap(); // styles start
        for &off in &offsets {
            out.write_u32::<LittleEndian>(off).unwrap();
        }
        out.write_all(&string_data).unwrap();
        out
    }

    fn encode_resource_ids(&self) -> Vec<u8> {
        if self.resource_ids.is_empty() {
            return Vec::new();
        }
        let chunk_size = 8 + self.resource_ids.len() as u32 * 4;
        let mut out = Vec::new();
        out.write_u16::<LittleEndian>(0x0180).unwrap(); // type
        out.write_u16::<LittleEndian>(8).unwrap(); // header size
        out.write_u32::<LittleEndian>(chunk_size).unwrap();
        for &rid in &self.resource_ids {
            out.write_u32::<LittleEndian>(rid).unwrap();
        }
        out
    }

    fn encode_element(&self, elem: &Element) -> Vec<u8> {
        let mut out = Vec::new();
        // Element start chunk.
        let ns_idx = elem
            .namespace
            .as_ref()
            .and_then(|n| self.string_map.get(n.as_str()))
            .copied()
            .map(|i| i as i32)
            .unwrap_or(-1);
        let name_idx = self.string_map[&elem.name];

        let attr_count = elem.attributes.len() as u16;
        let chunk_size: u32 = 36 + attr_count as u32 * 20;

        out.write_u16::<LittleEndian>(0x0102).unwrap(); // type: element start
        out.write_u16::<LittleEndian>(16).unwrap(); // header size
        out.write_u32::<LittleEndian>(chunk_size).unwrap();
        out.write_u32::<LittleEndian>(1).unwrap(); // line number
        out.write_i32::<LittleEndian>(-1).unwrap(); // comment
        out.write_i32::<LittleEndian>(ns_idx).unwrap(); // namespace
        out.write_u32::<LittleEndian>(name_idx).unwrap(); // name
        out.write_u16::<LittleEndian>(0x14).unwrap(); // attribute start
        out.write_u16::<LittleEndian>(0x14).unwrap(); // attribute size
        out.write_u16::<LittleEndian>(attr_count).unwrap();
        out.write_u16::<LittleEndian>(0).unwrap(); // id index
        out.write_u16::<LittleEndian>(0).unwrap(); // class index
        out.write_u16::<LittleEndian>(0).unwrap(); // style index

        for attr in &elem.attributes {
            let attr_ns = attr
                .namespace
                .as_ref()
                .and_then(|n| self.string_map.get(n.as_str()))
                .copied()
                .map(|i| i as i32)
                .unwrap_or(-1);
            let attr_name = self.string_map[&attr.name] as i32;
            let (raw_value, typed_type, typed_data) = match &attr.value {
                AttributeValue::String(s) => {
                    let sid = self.string_map[s.as_str()] as i32;
                    (sid, 0x03u8, sid as u32) // TYPE_STRING
                }
                AttributeValue::Integer(v) => (-1i32, 0x10u8, *v as u32), // TYPE_INT_DEC
                AttributeValue::Boolean(b) => (-1i32, 0x12u8, if *b { 0xFFFF_FFFFu32 } else { 0 }),
                AttributeValue::Reference(r) => (-1i32, 0x01u8, *r), // TYPE_REFERENCE
            };
            out.write_i32::<LittleEndian>(attr_ns).unwrap();
            out.write_i32::<LittleEndian>(attr_name).unwrap();
            out.write_i32::<LittleEndian>(raw_value).unwrap();
            // Typed value: size (2), res0 (1), type (1), data (4)
            out.write_u16::<LittleEndian>(8).unwrap(); // size
            out.write_u8(0).unwrap(); // res0
            out.write_u8(typed_type).unwrap(); // type
            out.write_u32::<LittleEndian>(typed_data).unwrap();
        }

        // Recurse into children.
        for child in &elem.children {
            out.write_all(&self.encode_element(child)).unwrap();
        }

        // Element end chunk.
        out.write_u16::<LittleEndian>(0x0103).unwrap(); // type: element end
        out.write_u16::<LittleEndian>(16).unwrap(); // header size
        out.write_u32::<LittleEndian>(24).unwrap(); // chunk size
        out.write_u32::<LittleEndian>(1).unwrap(); // line number
        out.write_i32::<LittleEndian>(-1).unwrap(); // comment
        out.write_i32::<LittleEndian>(ns_idx).unwrap(); // namespace
        out.write_u32::<LittleEndian>(name_idx).unwrap(); // name

        out
    }
}

fn encode_namespace(chunk_type: u16, prefix_idx: u32, uri_idx: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.write_u16::<LittleEndian>(chunk_type).unwrap();
    out.write_u16::<LittleEndian>(16).unwrap(); // header size
    out.write_u32::<LittleEndian>(24).unwrap(); // chunk size
    out.write_u32::<LittleEndian>(1).unwrap(); // line number
    out.write_i32::<LittleEndian>(-1).unwrap(); // comment
    out.write_u32::<LittleEndian>(prefix_idx).unwrap();
    out.write_u32::<LittleEndian>(uri_idx).unwrap();
    out
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_minimal_manifest() {
        let manifest = build_manifest("com.example.hello", 1, "1.0", 24, 34, None);
        let bytes = encode_axml(&manifest);
        // Check file header magic.
        assert_eq!(bytes[0], 0x03);
        assert_eq!(bytes[1], 0x00);
        // Check total size matches.
        let total = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(total as usize, bytes.len());
        // Verify it's not empty.
        assert!(bytes.len() > 100);
    }

    #[test]
    fn encode_manifest_with_activity() {
        let manifest = build_manifest("com.example.hello", 1, "1.0", 24, 34, Some(".MainActivity"));
        let bytes = encode_axml(&manifest);
        let total = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        assert_eq!(total as usize, bytes.len());
        // Should be larger than the minimal manifest due to activity +
        // intent-filter elements.
        assert!(bytes.len() > 300);
    }

    #[test]
    fn string_pool_contains_package() {
        let manifest = build_manifest("com.example.test", 1, "1.0", 24, 34, None);
        let bytes = encode_axml(&manifest);
        // The package name should appear somewhere in the binary.
        let pkg = b"com.example.test";
        assert!(
            bytes.windows(pkg.len()).any(|w| w == pkg),
            "package name not found in binary AXML"
        );
    }
}
