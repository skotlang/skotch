//! Binary XML (AXML) parser: the inverse of [`super::flatten`].
//!
//! Port of `xml::Inflate` (XmlDom.cpp) over `android::ResXMLTree`:
//! reconstructs an [`Element`] tree from compiled binary XML, keeping
//! both the string form and the typed value of each attribute.

use super::flatten::{
    RES_STRING_POOL_TYPE, RES_XML_CDATA_TYPE, RES_XML_END_ELEMENT_TYPE, RES_XML_END_NAMESPACE_TYPE,
    RES_XML_RESOURCE_MAP_TYPE, RES_XML_START_ELEMENT_TYPE, RES_XML_START_NAMESPACE_TYPE,
    RES_XML_TYPE,
};
use super::{AaptAttribute, Element, NamespaceDecl, Node, Text, XmlAttribute};
use crate::res::string_pool::BinaryStringPool;
use crate::res::value::{
    format, res_value_type, Attribute as AttrDef, Item, Reference, ReferenceType, ResValue,
};
use crate::res::ResourceId;
use anyhow::{anyhow, bail, Result};

const NULL_INDEX: u32 = 0xffff_ffff;

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

struct PendingNamespace {
    decl: NamespaceDecl,
    /// Whether the declaration has already been attached to the element
    /// that follows it. Namespace declarations attach only to the next
    /// START_ELEMENT (mirroring `XmlDom`'s pending element), not to
    /// every element in their scope.
    attached: bool,
}

/// Parses complete binary XML bytes into an element tree.
pub fn parse_binary_xml(data: &[u8]) -> Result<Element> {
    let chunk_type = read_u16(data, 0).ok_or_else(|| anyhow!("truncated binary XML"))?;
    if chunk_type != RES_XML_TYPE {
        bail!("not binary XML: leading chunk type 0x{chunk_type:04x}");
    }
    let header_size = read_u16(data, 2).ok_or_else(|| anyhow!("truncated binary XML"))? as usize;
    let total_size = read_u32(data, 4).ok_or_else(|| anyhow!("truncated binary XML"))? as usize;
    let total_size = total_size.min(data.len());

    let mut pool: Option<BinaryStringPool> = None;
    let mut resource_map: Vec<u32> = Vec::new();

    let mut root: Option<Element> = None;
    let mut stack: Vec<Element> = Vec::new();
    let mut pending_namespaces: Vec<PendingNamespace> = Vec::new();

    let mut offset = header_size;
    while offset + 8 <= total_size {
        let chunk_type = read_u16(data, offset).unwrap_or(0);
        let chunk_header_size = read_u16(data, offset + 2).unwrap_or(0) as usize;
        let chunk_size = read_u32(data, offset + 4).unwrap_or(0) as usize;
        if chunk_size < 8 || offset + chunk_size > total_size {
            break;
        }
        let chunk = &data[offset..offset + chunk_size];

        match chunk_type {
            RES_STRING_POOL_TYPE => {
                if pool.is_none() {
                    pool = BinaryStringPool::parse(chunk);
                }
            }
            RES_XML_RESOURCE_MAP_TYPE => {
                let mut map_offset = chunk_header_size;
                while map_offset + 4 <= chunk_size {
                    resource_map.push(read_u32(chunk, map_offset).unwrap_or(0));
                    map_offset += 4;
                }
            }
            RES_XML_START_NAMESPACE_TYPE => {
                let line = read_u32(chunk, 8).unwrap_or(0) as usize;
                let prefix_idx = read_u32(chunk, chunk_header_size).unwrap_or(NULL_INDEX);
                let uri_idx = read_u32(chunk, chunk_header_size + 4).unwrap_or(NULL_INDEX);
                pending_namespaces.push(PendingNamespace {
                    decl: NamespaceDecl {
                        prefix: pool_string(&pool, prefix_idx),
                        uri: pool_string(&pool, uri_idx),
                        line_number: line,
                        column_number: 0,
                    },
                    attached: false,
                });
            }
            RES_XML_END_NAMESPACE_TYPE => {
                pending_namespaces.pop();
            }
            RES_XML_START_ELEMENT_TYPE => {
                let line = read_u32(chunk, 8).unwrap_or(0) as usize;
                let ext = chunk_header_size;
                let ns_idx = read_u32(chunk, ext).unwrap_or(NULL_INDEX);
                let name_idx = read_u32(chunk, ext + 4).unwrap_or(NULL_INDEX);
                let attribute_start = read_u16(chunk, ext + 8).unwrap_or(20) as usize;
                let attribute_size = read_u16(chunk, ext + 10).unwrap_or(20) as usize;
                let attribute_count = read_u16(chunk, ext + 12).unwrap_or(0) as usize;

                let mut element = Element::new(pool_string(&pool, name_idx));
                element.namespace_uri = pool_string(&pool, ns_idx);
                element.line_number = line;
                // Namespaces declared since the previous element become
                // this element's declarations (mirrors XmlDom's pending
                // element). Already-attached declarations stay with the
                // element that introduced them.
                for pending in pending_namespaces.iter_mut() {
                    if !pending.attached {
                        element.namespace_decls.push(pending.decl.clone());
                        pending.attached = true;
                    }
                }

                for i in 0..attribute_count {
                    let attr_offset = ext + attribute_start + i * attribute_size;
                    let Some(attr) = parse_attribute(chunk, attr_offset, &pool, &resource_map)
                    else {
                        continue;
                    };
                    element.attributes.push(attr);
                }
                stack.push(element);
            }
            RES_XML_END_ELEMENT_TYPE => {
                if let Some(element) = stack.pop() {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(Node::Element(element)),
                        None => {
                            if root.is_none() {
                                root = Some(element);
                            }
                        }
                    }
                }
            }
            RES_XML_CDATA_TYPE => {
                let line = read_u32(chunk, 8).unwrap_or(0) as usize;
                let data_idx = read_u32(chunk, chunk_header_size).unwrap_or(NULL_INDEX);
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node::Text(Text {
                        text: pool_string(&pool, data_idx),
                        line_number: line,
                        column_number: 0,
                    }));
                }
            }
            _ => {}
        }
        offset += chunk_size;
    }

    root.ok_or_else(|| anyhow!("binary XML has no root element"))
}

fn pool_string(pool: &Option<BinaryStringPool>, index: u32) -> String {
    if index == NULL_INDEX {
        return String::new();
    }
    pool.as_ref()
        .and_then(|p| p.get(index as usize))
        .unwrap_or_default()
}

fn parse_attribute(
    chunk: &[u8],
    offset: usize,
    pool: &Option<BinaryStringPool>,
    resource_map: &[u32],
) -> Option<XmlAttribute> {
    let ns_idx = read_u32(chunk, offset)?;
    let name_idx = read_u32(chunk, offset + 4)?;
    let raw_value_idx = read_u32(chunk, offset + 8)?;
    // typedValue: size(2) res0(1) dataType(1) data(4)
    let data_type = *chunk.get(offset + 15)?;
    let data = read_u32(chunk, offset + 16)?;

    let mut attr = XmlAttribute::new(
        pool_string(pool, ns_idx),
        pool_string(pool, name_idx),
        String::new(),
    );

    // The resource map is indexed by string-pool index.
    if (name_idx as usize) < resource_map.len() {
        let id = ResourceId(resource_map[name_idx as usize]);
        if id.is_valid() {
            attr.compiled_attribute = Some(AaptAttribute {
                attribute: AttrDef::new(format::ANY),
                id: Some(id),
            });
        }
    }

    if raw_value_idx != NULL_INDEX {
        attr.value = pool_string(pool, raw_value_idx);
    }

    use res_value_type::*;
    attr.compiled_value = Some(match data_type {
        TYPE_STRING => {
            let s = pool_string(pool, data);
            if attr.value.is_empty() {
                attr.value = s.clone();
            }
            Item::String {
                value: s,
                untranslatable_sections: vec![],
            }
        }
        TYPE_REFERENCE | TYPE_DYNAMIC_REFERENCE => Item::Reference(Reference {
            id: Some(ResourceId(data)),
            is_dynamic: data_type == TYPE_DYNAMIC_REFERENCE,
            ..Default::default()
        }),
        TYPE_ATTRIBUTE | TYPE_DYNAMIC_ATTRIBUTE => Item::Reference(Reference {
            id: Some(ResourceId(data)),
            reference_type: ReferenceType::Attribute,
            is_dynamic: data_type == TYPE_DYNAMIC_ATTRIBUTE,
            ..Default::default()
        }),
        other => Item::BinaryPrimitive(ResValue::new(other, data)),
    });

    Some(attr)
}

#[cfg(test)]
mod tests {
    use super::super::flatten::{flatten_xml, XmlFlattenerOptions};
    use super::super::parse_source_xml;
    use super::*;

    #[test]
    fn round_trip_with_typed_values() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns:a="http://schemas.android.com/apk/res/android" a:x="hello" plain="world">
  some text
  <child/>
</root>"#;
        let parsed = parse_source_xml("t.xml", xml).unwrap();
        let bytes = flatten_xml(&parsed.root.unwrap(), &XmlFlattenerOptions::default());
        let reparsed = parse_binary_xml(&bytes).unwrap();

        assert_eq!(reparsed.name, "root");
        assert_eq!(reparsed.namespace_decls.len(), 1);
        assert_eq!(reparsed.namespace_decls[0].prefix, "a");
        assert_eq!(
            reparsed.attr_value("http://schemas.android.com/apk/res/android", "x"),
            Some("hello")
        );
        assert_eq!(reparsed.attr_value("", "plain"), Some("world"));
        // Whitespace-padded text is compacted to single spaces.
        assert_eq!(reparsed.text(), " some text ");
        assert!(reparsed.find_child("", "child").is_some());
    }
}
