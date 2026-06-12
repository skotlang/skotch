//! Binary XML (AXML) flattener.
//!
//! Port of `format/binary/XmlFlattener.cpp`: turns an [`Element`] tree
//! into the binary XML format Android's runtime parses
//! (`AndroidManifest.xml`, compiled layouts, …).

use super::{Element, Node, XmlAttribute, SCHEMA_TOOLS};
use crate::res::string_pool::{Context, Ref, StringPool};
use crate::res::value::{res_value_type, Item, ResValue};
use crate::res::ResourceId;
use crate::util::process_string_preserve_spaces;

pub const RES_XML_TYPE: u16 = 0x0003;
pub const RES_STRING_POOL_TYPE: u16 = 0x0001;
pub const RES_XML_START_NAMESPACE_TYPE: u16 = 0x0100;
pub const RES_XML_END_NAMESPACE_TYPE: u16 = 0x0101;
pub const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
pub const RES_XML_END_ELEMENT_TYPE: u16 = 0x0103;
pub const RES_XML_CDATA_TYPE: u16 = 0x0104;
pub const RES_XML_RESOURCE_MAP_TYPE: u16 = 0x0180;

const LOW_PRIORITY: u32 = 0xffff_ffff;
const NULL_INDEX: u32 = 0xffff_ffff;
/// `android:id` — gets a dedicated index slot in element headers.
const ID_ATTR: u32 = 0x0101_00d0;

#[derive(Debug, Clone, Copy, Default)]
pub struct XmlFlattenerOptions {
    /// Keep raw string values of attributes that have compiled values
    /// (`--keep-raw-values`).
    pub keep_raw_values: bool,
    /// Encode the string pool as UTF-16 (used for static libraries).
    pub use_utf16: bool,
}

/// A pending 4-byte string-index slot to patch after the pool is sorted.
struct StringRef {
    /// Byte offset into the node buffer.
    offset: usize,
    /// 0 = main pool; n > 0 = package pool n-1.
    pool: usize,
    reference: Ref,
}

struct Flattener {
    nodes: Vec<u8>,
    pool: StringPool,
    /// Attribute-name pools per package ID (merged into `pool` at the
    /// end without deduplication, mirroring aapt2).
    package_pools: Vec<(u8, StringPool)>,
    string_refs: Vec<StringRef>,
    options: XmlFlattenerOptions,
}

impl Flattener {
    fn new(options: XmlFlattenerOptions) -> Self {
        Flattener {
            nodes: Vec::new(),
            pool: StringPool::new(),
            package_pools: Vec::new(),
            string_refs: Vec::new(),
            options,
        }
    }

    fn push_u16(&mut self, v: u16) {
        self.nodes.extend_from_slice(&v.to_le_bytes());
    }

    fn push_u32(&mut self, v: u32) {
        self.nodes.extend_from_slice(&v.to_le_bytes());
    }

    /// Reserves a string-index slot, treating empty strings as null when
    /// requested (the runtime distinguishes null from "").
    fn add_string(&mut self, s: &str, treat_empty_as_null: bool) {
        if treat_empty_as_null && s.is_empty() {
            self.push_u32(NULL_INDEX);
            return;
        }
        let reference = self.pool.make_ref(s, Context::with_priority(LOW_PRIORITY));
        self.string_refs.push(StringRef { offset: self.nodes.len(), pool: 0, reference });
        self.push_u32(0);
    }

    /// Writes a `ResXMLTree_node` header; returns the offset of the
    /// chunk-size field for patching.
    fn start_node(&mut self, chunk_type: u16, line_number: usize) -> usize {
        self.push_u16(chunk_type);
        self.push_u16(16); // header size: ResChunk_header + line + comment
        let size_offset = self.nodes.len();
        self.push_u32(0); // patched in end_node
        self.push_u32(line_number as u32);
        self.push_u32(NULL_INDEX); // comment
        size_offset
    }

    fn end_node(&mut self, size_offset: usize) {
        let size = (self.nodes.len() - size_offset + 4) as u32;
        self.nodes[size_offset..size_offset + 4].copy_from_slice(&size.to_le_bytes());
    }

    fn write_namespace(&mut self, prefix: &str, uri: &str, line: usize, chunk_type: u16) {
        let size_offset = self.start_node(chunk_type, line);
        self.add_string(prefix, false);
        self.add_string(uri, false);
        self.end_node(size_offset);
    }

    fn visit_text(&mut self, text: &str, line_number: usize) {
        let trimmed = crate::util::trim_whitespace(text);
        // Skip whitespace-only text nodes.
        if trimmed.is_empty() {
            return;
        }
        // Compact leading and trailing whitespace into a single space.
        let mut compacted = String::new();
        if text.starts_with(|c: char| c.is_ascii_whitespace()) {
            compacted.push(' ');
        }
        compacted.push_str(trimmed);
        if text.ends_with(|c: char| c.is_ascii_whitespace()) {
            compacted.push(' ');
        }

        let size_offset = self.start_node(RES_XML_CDATA_TYPE, line_number);
        self.add_string(&compacted, false);
        // typedData: zeroed Res_value, as aapt2 leaves it.
        self.push_u16(0);
        self.push_u16(0);
        self.push_u32(0);
        self.end_node(size_offset);
    }

    fn visit_element(&mut self, element: &Element) {
        for decl in &element.namespace_decls {
            if decl.uri != SCHEMA_TOOLS {
                self.write_namespace(
                    &decl.prefix,
                    &decl.uri,
                    decl.line_number,
                    RES_XML_START_NAMESPACE_TYPE,
                );
            }
        }

        self.write_start_element(element);

        for child in &element.children {
            match child {
                Node::Element(el) => self.visit_element(el),
                Node::Text(text) => self.visit_text(&text.text, text.line_number),
            }
        }

        // End element.
        let size_offset = self.start_node(RES_XML_END_ELEMENT_TYPE, element.line_number);
        self.add_string(&element.namespace_uri, true);
        self.add_string(&element.name, false);
        self.end_node(size_offset);

        for decl in element.namespace_decls.iter().rev() {
            if decl.uri != SCHEMA_TOOLS {
                self.write_namespace(
                    &decl.prefix,
                    &decl.uri,
                    decl.line_number,
                    RES_XML_END_NAMESPACE_TYPE,
                );
            }
        }
    }

    fn write_start_element(&mut self, element: &Element) {
        let size_offset = self.start_node(RES_XML_START_ELEMENT_TYPE, element.line_number);
        self.add_string(&element.namespace_uri, true);
        self.add_string(&element.name, true);

        // attrExt: attributeStart(2) attributeSize(2) attributeCount(2)
        // idIndex(2) classIndex(2) styleIndex(2)
        self.push_u16(20); // sizeof(ResXMLTree_attrExt)
        self.push_u16(20); // sizeof(ResXMLTree_attribute)

        let mut filtered: Vec<&XmlAttribute> = element
            .attributes
            .iter()
            .filter(|a| a.namespace_uri != SCHEMA_TOOLS)
            .collect();
        filtered.sort_by(|a, b| cmp_xml_attribute_by_id(a, b));

        self.push_u16(filtered.len() as u16);

        let mut id_index = 0u16;
        let mut class_index = 0u16;
        let mut style_index = 0u16;
        for (i, attr) in filtered.iter().enumerate() {
            let attribute_index = (i + 1) as u16;
            let attr_id = attr.compiled_attribute.as_ref().and_then(|c| c.id);
            if attr_id == Some(ResourceId(ID_ATTR)) {
                id_index = attribute_index;
            } else if attr.namespace_uri.is_empty() {
                if attr.name == "class" {
                    class_index = attribute_index;
                } else if attr.name == "style" {
                    style_index = attribute_index;
                }
            }
        }
        self.push_u16(id_index);
        self.push_u16(class_index);
        self.push_u16(style_index);

        for attr in &filtered {
            self.write_attribute(attr);
        }

        self.end_node(size_offset);
    }

    fn write_attribute(&mut self, attr: &XmlAttribute) {
        self.add_string(&attr.namespace_uri, true);

        // Name: attributes with resource IDs go into per-package pools
        // with the resource ID as priority, so they sort to the front
        // and line up with the resource-map array.
        let attr_id = attr.compiled_attribute.as_ref().and_then(|c| c.id);
        match attr_id {
            Some(id) => {
                let package_id = id.package_id();
                let pool_index = match self
                    .package_pools
                    .iter()
                    .position(|(p, _)| *p == package_id)
                {
                    Some(i) => i,
                    None => {
                        self.package_pools.push((package_id, StringPool::new()));
                        self.package_pools.len() - 1
                    }
                };
                let reference = self.package_pools[pool_index]
                    .1
                    .make_ref(&attr.name, Context::with_priority(id.0));
                self.string_refs.push(StringRef {
                    offset: self.nodes.len(),
                    pool: pool_index + 1,
                    reference,
                });
                self.push_u32(0);
            }
            None => self.add_string(&attr.name, false),
        }

        // rawValue placeholder; may be patched below.
        let raw_value_offset = self.nodes.len();
        self.push_u32(NULL_INDEX);

        // typedValue: size(2) res0(1) dataType(1) data(4)
        let mut compiled_text: Option<String> = None;
        let mut typed: Option<ResValue> = None;
        match &attr.compiled_value {
            Some(Item::String { value, .. }) => compiled_text = Some(value.clone()),
            Some(item) => {
                typed = item.flatten();
                if typed.is_none() {
                    // Raw strings or file references degrade to text.
                    compiled_text = Some(match item {
                        Item::RawString(s) => s.clone(),
                        Item::FileReference(f) => f.path.clone(),
                        _ => String::new(),
                    });
                }
            }
            None => {
                compiled_text = Some(process_string_preserve_spaces(&attr.value));
            }
        }

        self.push_u16(8); // typedValue.size
        match (&compiled_text, typed) {
            (Some(text), _) => {
                self.push_u16((res_value_type::TYPE_STRING as u16) << 8); // res0=0, dataType
                self.add_string(text, false);
                // Patch rawValue: raw original with keep_raw_values,
                // otherwise the same compiled text.
                let raw = if self.options.keep_raw_values { &attr.value } else { text };
                let reference = self.pool.make_ref(raw, Context::with_priority(LOW_PRIORITY));
                self.string_refs.push(StringRef { offset: raw_value_offset, pool: 0, reference });
                self.nodes[raw_value_offset..raw_value_offset + 4]
                    .copy_from_slice(&0u32.to_le_bytes());
            }
            (None, Some(value)) => {
                self.push_u16((value.data_type as u16) << 8);
                self.push_u32(value.data);
                if self.options.keep_raw_values && !attr.value.is_empty() {
                    let reference =
                        self.pool.make_ref(&attr.value, Context::with_priority(LOW_PRIORITY));
                    self.string_refs.push(StringRef {
                        offset: raw_value_offset,
                        pool: 0,
                        reference,
                    });
                    self.nodes[raw_value_offset..raw_value_offset + 4]
                        .copy_from_slice(&0u32.to_le_bytes());
                }
            }
            (None, None) => {
                // Unreachable: one of the two is always set above.
                self.push_u16(0);
                self.push_u32(0);
            }
        }
    }
}

fn cmp_xml_attribute_by_id(a: &XmlAttribute, b: &XmlAttribute) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a_id = a.compiled_attribute.as_ref().and_then(|c| c.id);
    let b_id = b.compiled_attribute.as_ref().and_then(|c| c.id);
    match (a_id, b_id) {
        (Some(a_id), Some(b_id)) => a_id.cmp(&b_id),
        (Some(_), None) => Ordering::Less,
        (None, _) => {
            if b.compiled_attribute.is_none() {
                (&a.namespace_uri, &a.name).cmp(&(&b.namespace_uri, &b.name))
            } else {
                Ordering::Greater
            }
        }
    }
}

/// Flattens an element tree to complete binary XML bytes.
/// Port of `XmlFlattener::Flatten`.
pub fn flatten_xml(root: &Element, options: &XmlFlattenerOptions) -> Vec<u8> {
    let mut flattener = Flattener::new(*options);
    flattener.visit_element(root);

    let Flattener { nodes, mut pool, package_pools, string_refs, .. } = flattener;

    // Merge package pools into the main pool (no dedupe), remembering
    // how each sub-pool's refs translate.
    let mut merged_offsets = vec![0usize; package_pools.len() + 1];
    for (i, (_, package_pool)) in package_pools.into_iter().enumerate() {
        merged_offsets[i + 1] = pool.merge(package_pool);
    }

    // Sort so attribute names with resource IDs (small priorities) come
    // first; the resource map is indexed by string-pool order.
    pool.sort();

    // Patch all string indices.
    let mut nodes = nodes;
    for string_ref in &string_refs {
        let reference = string_ref.reference.offset_by(merged_offsets[string_ref.pool]);
        let index = pool.resolve(reference);
        nodes[string_ref.offset..string_ref.offset + 4]
            .copy_from_slice(&(index as u32).to_le_bytes());
    }

    // Resource map: resource IDs of the leading pool entries whose
    // priority is a valid resource ID.
    let mut resource_map: Vec<u32> = Vec::new();
    for priority in pool.priorities() {
        let id = ResourceId(priority);
        if priority == LOW_PRIORITY || !id.is_valid() {
            break;
        }
        resource_map.push(id.0);
    }

    let pool_chunk = if options.use_utf16 { pool.flatten_utf16() } else { pool.flatten_utf8() };

    // Assemble: RES_XML_TYPE header + string pool + resource map + nodes.
    let mut out = Vec::new();
    out.extend_from_slice(&RES_XML_TYPE.to_le_bytes());
    out.extend_from_slice(&8u16.to_le_bytes());
    let total_size_offset = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pool_chunk);
    if !resource_map.is_empty() {
        out.extend_from_slice(&RES_XML_RESOURCE_MAP_TYPE.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&((8 + 4 * resource_map.len()) as u32).to_le_bytes());
        for id in &resource_map {
            out.extend_from_slice(&id.to_le_bytes());
        }
    }
    out.extend_from_slice(&nodes);
    let total = out.len() as u32;
    out[total_size_offset..total_size_offset + 4].copy_from_slice(&total.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse_source_xml;

    #[test]
    fn flatten_and_reparse() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.app">
    <application android:label="App">
        <activity android:name=".Main"/>
    </application>
</manifest>"#;
        let res = parse_source_xml("AndroidManifest.xml", xml).unwrap();
        let root = res.root.unwrap();
        let bytes = flatten_xml(&root, &XmlFlattenerOptions::default());

        // Header sanity.
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), RES_XML_TYPE);
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize,
            bytes.len()
        );

        // Full round-trip through the binary XML parser.
        let reparsed = super::super::axml::parse_binary_xml(&bytes).unwrap();
        assert_eq!(reparsed.name, "manifest");
        assert_eq!(reparsed.attr_value("", "package"), Some("com.app"));
        let app = reparsed.find_child("", "application").unwrap();
        assert_eq!(
            app.attr_value(super::super::SCHEMA_ANDROID, "label"),
            Some("App")
        );
    }
}
