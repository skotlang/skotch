//! XML document model.
//!
//! Port of aapt2's `xml/XmlDom.{h,cpp}`: a DOM tailored to Android
//! resource processing. Source XML is parsed with `roxmltree` but
//! normalized to match aapt2's expat-based parser semantics:
//!
//! - element attributes are sorted by (namespace_uri, name, value);
//! - text nodes keep their whitespace verbatim, but empty text is
//!   dropped;
//! - a comment is attached to the element that follows it;
//! - namespace declarations are recorded on the element that declares
//!   them, in declaration order.

pub mod axml;
pub mod flatten;

use crate::pb::wire::{Reader, Writer, WIRE_LEN};
use crate::res::value::{Attribute as AttrDef, Item};
use crate::res::{ResourceId, Source};

/// Well-known Android XML namespace URIs.
pub const SCHEMA_AUTO: &str = "http://schemas.android.com/apk/res-auto";
pub const SCHEMA_PUBLIC_PREFIX: &str = "http://schemas.android.com/apk/res/";
pub const SCHEMA_PRIVATE_PREFIX: &str = "http://schemas.android.com/apk/prv/res/";
pub const SCHEMA_ANDROID: &str = "http://schemas.android.com/apk/res/android";
pub const SCHEMA_TOOLS: &str = "http://schemas.android.com/tools";
pub const SCHEMA_AAPT: &str = "http://schemas.android.com/aapt";

/// Result of interpreting a namespace URI as a package reference.
/// Mirrors `xml::ExtractedPackage`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedPackage {
    /// Empty means the app's own package.
    pub package: String,
    pub private_namespace: bool,
}

/// Interprets a namespace URI, returning the package it refers to.
/// Mirrors `xml::ExtractPackageFromNamespace`.
pub fn extract_package_from_namespace(namespace_uri: &str) -> Option<ExtractedPackage> {
    if let Some(package) = namespace_uri.strip_prefix(SCHEMA_PUBLIC_PREFIX) {
        if !package.is_empty() {
            return Some(ExtractedPackage { package: package.to_string(), private_namespace: false });
        }
    } else if let Some(package) = namespace_uri.strip_prefix(SCHEMA_PRIVATE_PREFIX) {
        if !package.is_empty() {
            return Some(ExtractedPackage { package: package.to_string(), private_namespace: true });
        }
    } else if namespace_uri == SCHEMA_AUTO {
        return Some(ExtractedPackage { package: String::new(), private_namespace: true });
    }
    None
}

/// Builds the namespace URI for a package, mirroring
/// `xml::BuildPackageNamespace`.
pub fn build_package_namespace(package: &str, private_reference: bool) -> String {
    let prefix = if private_reference { SCHEMA_PRIVATE_PREFIX } else { SCHEMA_PUBLIC_PREFIX };
    format!("{prefix}{package}")
}

/// A namespace declaration (`xmlns:prefix="uri"`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NamespaceDecl {
    pub prefix: String,
    pub uri: String,
    pub line_number: usize,
    pub column_number: usize,
}

/// A compiled attribute reference: the attribute definition (if found)
/// plus its resource ID. Mirrors `xml::AaptAttribute`.
#[derive(Debug, Clone, PartialEq)]
pub struct AaptAttribute {
    pub attribute: AttrDef,
    pub id: Option<ResourceId>,
}

/// An XML attribute, optionally with its compiled (typed) value.
#[derive(Debug, Clone, PartialEq)]
pub struct XmlAttribute {
    pub namespace_uri: String,
    pub name: String,
    pub value: String,
    /// The interpreted value, populated during linking.
    pub compiled_value: Option<Item>,
    /// The attribute definition this resolves to, populated during linking.
    pub compiled_attribute: Option<AaptAttribute>,
}

impl XmlAttribute {
    pub fn new(
        namespace_uri: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        XmlAttribute {
            namespace_uri: namespace_uri.into(),
            name: name.into(),
            value: value.into(),
            compiled_value: None,
            compiled_attribute: None,
        }
    }
}

/// A node: either an element or a text run.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Element),
    Text(Text),
}

impl Node {
    pub fn as_element(&self) -> Option<&Element> {
        match self {
            Node::Element(el) => Some(el),
            _ => None,
        }
    }

    pub fn as_element_mut(&mut self) -> Option<&mut Element> {
        match self {
            Node::Element(el) => Some(el),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Text {
    pub text: String,
    pub line_number: usize,
    pub column_number: usize,
}

/// An XML element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Element {
    pub namespace_decls: Vec<NamespaceDecl>,
    pub namespace_uri: String,
    pub name: String,
    pub attributes: Vec<XmlAttribute>,
    pub children: Vec<Node>,
    /// Comment immediately preceding this element.
    pub comment: String,
    pub line_number: usize,
    pub column_number: usize,
}

impl Element {
    pub fn new(name: impl Into<String>) -> Self {
        Element { name: name.into(), ..Default::default() }
    }

    pub fn find_attribute(&self, namespace_uri: &str, name: &str) -> Option<&XmlAttribute> {
        self.attributes
            .iter()
            .find(|a| a.namespace_uri == namespace_uri && a.name == name)
    }

    pub fn find_attribute_mut(
        &mut self,
        namespace_uri: &str,
        name: &str,
    ) -> Option<&mut XmlAttribute> {
        self.attributes
            .iter_mut()
            .find(|a| a.namespace_uri == namespace_uri && a.name == name)
    }

    pub fn attr_value(&self, namespace_uri: &str, name: &str) -> Option<&str> {
        self.find_attribute(namespace_uri, name).map(|a| a.value.as_str())
    }

    pub fn remove_attribute(&mut self, namespace_uri: &str, name: &str) -> Option<XmlAttribute> {
        let pos = self
            .attributes
            .iter()
            .position(|a| a.namespace_uri == namespace_uri && a.name == name)?;
        Some(self.attributes.remove(pos))
    }

    /// Sets (or replaces) an attribute value.
    pub fn set_attribute(&mut self, namespace_uri: &str, name: &str, value: &str) {
        match self.find_attribute_mut(namespace_uri, name) {
            Some(attr) => attr.value = value.to_string(),
            None => self.attributes.push(XmlAttribute::new(namespace_uri, name, value)),
        }
    }

    pub fn find_child(&self, namespace_uri: &str, name: &str) -> Option<&Element> {
        self.children.iter().filter_map(Node::as_element).find(|el| {
            el.namespace_uri == namespace_uri && el.name == name
        })
    }

    pub fn find_child_mut(&mut self, namespace_uri: &str, name: &str) -> Option<&mut Element> {
        self.children
            .iter_mut()
            .filter_map(Node::as_element_mut)
            .find(|el| el.namespace_uri == namespace_uri && el.name == name)
    }

    pub fn child_elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(Node::as_element)
    }

    pub fn child_elements_mut(&mut self) -> impl Iterator<Item = &mut Element> {
        self.children.iter_mut().filter_map(Node::as_element_mut)
    }

    /// Concatenated text of direct text children.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for child in &self.children {
            if let Node::Text(t) = child {
                out.push_str(&t.text);
            }
        }
        out
    }

    /// True if this element has no child elements and no non-whitespace
    /// text.
    pub fn is_empty(&self) -> bool {
        self.children.iter().all(|c| match c {
            Node::Element(_) => false,
            Node::Text(t) => t.text.trim().is_empty(),
        })
    }

    /// Source for diagnostics, given the containing file's path.
    pub fn source_in(&self, path: &str) -> Source {
        Source::with_line(path, self.line_number)
    }
}

/// A parsed XML document plus the source path it came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmlResource {
    pub source_path: String,
    pub root: Option<Element>,
}

/// Parses source XML into the aapt2-shaped DOM.
pub fn parse_source_xml(source_path: &str, text: &str) -> anyhow::Result<XmlResource> {
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options)
        .map_err(|e| anyhow::anyhow!("{source_path}:{}: {e}", e.pos().row))?;
    let root_node = doc.root_element();
    let root = convert_element(&doc, root_node, None)?;
    Ok(XmlResource { source_path: source_path.to_string(), root: Some(root) })
}

fn node_pos(doc: &roxmltree::Document, node: roxmltree::Node) -> (usize, usize) {
    let pos = doc.text_pos_at(node.range().start);
    (pos.row as usize, pos.col as usize)
}

fn convert_element(
    doc: &roxmltree::Document,
    node: roxmltree::Node,
    parent: Option<roxmltree::Node>,
) -> anyhow::Result<Element> {
    let mut element = Element::new(node.tag_name().name());
    element.namespace_uri = node.tag_name().namespace().unwrap_or("").to_string();
    let (line, col) = node_pos(doc, node);
    element.line_number = line;
    element.column_number = col;

    // Namespaces declared on this element: in-scope namespaces minus the
    // parent's in-scope namespaces.
    for ns in node.namespaces() {
        let in_parent = parent.is_some_and(|p| {
            p.namespaces()
                .any(|pn| pn.name() == ns.name() && pn.uri() == ns.uri())
        });
        if !in_parent {
            element.namespace_decls.push(NamespaceDecl {
                prefix: ns.name().unwrap_or("").to_string(),
                uri: ns.uri().to_string(),
                line_number: line,
                column_number: col,
            });
        }
    }

    for attr in node.attributes() {
        element.attributes.push(XmlAttribute::new(
            attr.namespace().unwrap_or(""),
            attr.name(),
            attr.value(),
        ));
    }
    // aapt2 sorts attributes at parse time.
    element
        .attributes
        .sort_by(|a, b| {
            (&a.namespace_uri, &a.name, &a.value).cmp(&(&b.namespace_uri, &b.name, &b.value))
        });

    // Children: elements and text; comments attach to the next element;
    // adjacent text runs merge; empty text drops.
    let mut pending_comment = String::new();
    let mut pending_text: Option<Text> = None;
    for child in node.children() {
        if child.is_element() {
            if let Some(text) = pending_text.take() {
                if !text.text.is_empty() {
                    element.children.push(Node::Text(text));
                }
            }
            let mut child_el = convert_element(doc, child, Some(node))?;
            child_el.comment = std::mem::take(&mut pending_comment);
            element.children.push(Node::Element(child_el));
        } else if child.is_text() {
            let chunk = child.text().unwrap_or("");
            if chunk.is_empty() {
                continue;
            }
            match &mut pending_text {
                Some(text) => text.text.push_str(chunk),
                None => {
                    let (line, col) = node_pos(doc, child);
                    pending_text = Some(Text {
                        text: chunk.to_string(),
                        line_number: line,
                        column_number: col,
                    });
                }
            }
        } else if child.is_comment() {
            if let Some(text) = pending_text.take() {
                if !text.text.is_empty() {
                    element.children.push(Node::Text(text));
                }
            }
            pending_comment = child.text().unwrap_or("").trim().to_string();
        }
    }
    if let Some(text) = pending_text {
        if !text.text.is_empty() {
            element.children.push(Node::Text(text));
        }
    }

    Ok(element)
}

// ───────────────────── proto XML (pb::XmlNode) ─────────────────────

/// Encodes a DOM as a serialized `aapt.pb.XmlNode` message.
/// Mirrors `SerializeXmlToPb`.
pub fn encode_pb_xml(root: &Element) -> Vec<u8> {
    let mut writer = Writer::new();
    encode_pb_node_into(&mut writer, root);
    writer.into_bytes()
}

fn encode_pb_node_into(writer: &mut Writer, element: &Element) {
    // XmlNode.element = 1
    writer.message(1, |w| encode_pb_element(w, element));
    if element.line_number != 0 || element.column_number != 0 {
        writer.message(3, |w| {
            w.varint(1, element.line_number as u64);
            w.varint(2, element.column_number as u64);
        });
    }
}

fn encode_pb_element(writer: &mut Writer, element: &Element) {
    for decl in &element.namespace_decls {
        writer.message(1, |w| {
            w.string(1, &decl.prefix);
            w.string(2, &decl.uri);
            if decl.line_number != 0 || decl.column_number != 0 {
                w.message(3, |sw| {
                    sw.varint(1, decl.line_number as u64);
                    sw.varint(2, decl.column_number as u64);
                });
            }
        });
    }
    writer.string(2, &element.namespace_uri);
    writer.string(3, &element.name);
    for attr in &element.attributes {
        writer.message(4, |w| {
            w.string(1, &attr.namespace_uri);
            w.string(2, &attr.name);
            w.string(3, &attr.value);
            // Source position (4) is omitted for attributes, matching
            // aapt2 which only serializes it when meaningful.
            if let Some(compiled) = &attr.compiled_attribute {
                if let Some(id) = compiled.id {
                    w.varint(5, id.0 as u64);
                }
            }
            if let Some(item) = &attr.compiled_value {
                w.message(6, |iw| crate::pb::encode_item(iw, item));
            }
        });
    }
    for child in &element.children {
        match child {
            Node::Element(el) => {
                writer.message(5, |w| {
                    w.message(1, |ew| encode_pb_element(ew, el));
                    if el.line_number != 0 || el.column_number != 0 {
                        w.message(3, |sw| {
                            sw.varint(1, el.line_number as u64);
                            sw.varint(2, el.column_number as u64);
                        });
                    }
                });
            }
            Node::Text(text) => {
                writer.message(5, |w| {
                    w.string(2, &text.text);
                    if text.line_number != 0 || text.column_number != 0 {
                        w.message(3, |sw| {
                            sw.varint(1, text.line_number as u64);
                            sw.varint(2, text.column_number as u64);
                        });
                    }
                });
            }
        }
    }
}

/// Decodes a serialized `aapt.pb.XmlNode` into a DOM. The root must be
/// an element. Mirrors `DeserializeXmlFromPb`.
pub fn decode_pb_xml(data: &[u8]) -> anyhow::Result<Element> {
    let (element, _text) = decode_pb_node(data)?;
    element.ok_or_else(|| anyhow::anyhow!("root XmlNode must be an element"))
}

fn decode_pb_node(data: &[u8]) -> anyhow::Result<(Option<Element>, Option<Text>)> {
    let mut element = None;
    let mut text: Option<Text> = None;
    let mut line = 0usize;
    let mut col = 0usize;
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => element = Some(decode_pb_element(field.data)?),
            (2, WIRE_LEN) => {
                text = Some(Text { text: field.as_string(), ..Default::default() })
            }
            (3, WIRE_LEN) => {
                let mut sub = Reader::new(field.data);
                while let Some(pos_field) = sub.next_field() {
                    match pos_field.number {
                        1 => line = pos_field.value as usize,
                        2 => col = pos_field.value as usize,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(el) = &mut element {
        el.line_number = line;
        el.column_number = col;
    }
    if let Some(t) = &mut text {
        t.line_number = line;
        t.column_number = col;
    }
    Ok((element, text))
}

fn decode_pb_element(data: &[u8]) -> anyhow::Result<Element> {
    let mut element = Element::default();
    let mut reader = Reader::new(data);
    while let Some(field) = reader.next_field() {
        match (field.number, field.wire_type) {
            (1, WIRE_LEN) => {
                let mut decl = NamespaceDecl::default();
                let mut sub = Reader::new(field.data);
                while let Some(ns_field) = sub.next_field() {
                    match (ns_field.number, ns_field.wire_type) {
                        (1, WIRE_LEN) => decl.prefix = ns_field.as_string(),
                        (2, WIRE_LEN) => decl.uri = ns_field.as_string(),
                        (3, WIRE_LEN) => {
                            let mut pos = Reader::new(ns_field.data);
                            while let Some(pos_field) = pos.next_field() {
                                match pos_field.number {
                                    1 => decl.line_number = pos_field.value as usize,
                                    2 => decl.column_number = pos_field.value as usize,
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                element.namespace_decls.push(decl);
            }
            (2, WIRE_LEN) => element.namespace_uri = field.as_string(),
            (3, WIRE_LEN) => element.name = field.as_string(),
            (4, WIRE_LEN) => {
                let mut attr = XmlAttribute::new("", "", "");
                let mut sub = Reader::new(field.data);
                while let Some(attr_field) = sub.next_field() {
                    match (attr_field.number, attr_field.wire_type) {
                        (1, WIRE_LEN) => attr.namespace_uri = attr_field.as_string(),
                        (2, WIRE_LEN) => attr.name = attr_field.as_string(),
                        (3, WIRE_LEN) => attr.value = attr_field.as_string(),
                        (5, _) => {
                            let id = ResourceId(attr_field.as_u32());
                            attr.compiled_attribute = Some(AaptAttribute {
                                attribute: AttrDef::new(crate::res::value::format::ANY),
                                id: if id.0 != 0 { Some(id) } else { None },
                            });
                        }
                        (6, WIRE_LEN) => {
                            attr.compiled_value = crate::pb::decode_item(attr_field.data)?;
                        }
                        _ => {}
                    }
                }
                element.attributes.push(attr);
            }
            (5, WIRE_LEN) => {
                let (child_el, child_text) = decode_pb_node(field.data)?;
                if let Some(el) = child_el {
                    element.children.push(Node::Element(el));
                } else if let Some(t) = child_text {
                    element.children.push(Node::Text(t));
                }
            }
            _ => {}
        }
    }
    Ok(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_manifest() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<!-- top comment -->
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    package="com.app">
    <application android:label="@string/app_name">
        <activity android:name=".Main"/>
    </application>
</manifest>"#;
        let res = parse_source_xml("AndroidManifest.xml", xml).unwrap();
        let root = res.root.unwrap();
        assert_eq!(root.name, "manifest");
        assert_eq!(root.namespace_decls.len(), 1);
        assert_eq!(root.namespace_decls[0].prefix, "android");
        assert_eq!(root.namespace_decls[0].uri, SCHEMA_ANDROID);
        assert_eq!(root.attr_value("", "package"), Some("com.app"));
        let app = root.find_child("", "application").unwrap();
        assert_eq!(app.attr_value(SCHEMA_ANDROID, "label"), Some("@string/app_name"));
        assert!(app.find_child("", "activity").is_some());
    }

    #[test]
    fn comment_attaches_to_next_element() {
        let xml = r#"<root><!-- hi --><child/></root>"#;
        let res = parse_source_xml("t.xml", xml).unwrap();
        let root = res.root.unwrap();
        let child = root.find_child("", "child").unwrap();
        assert_eq!(child.comment, "hi");
    }

    #[test]
    fn attributes_sorted() {
        let xml = r#"<a zeta="1" alpha="2"/>"#;
        let res = parse_source_xml("t.xml", xml).unwrap();
        let root = res.root.unwrap();
        assert_eq!(root.attributes[0].name, "alpha");
        assert_eq!(root.attributes[1].name, "zeta");
    }

    #[test]
    fn pb_round_trip() {
        let xml = r#"<root xmlns:android="http://schemas.android.com/apk/res/android">
  <child android:value="x">text</child>
</root>"#;
        let res = parse_source_xml("t.xml", xml).unwrap();
        let root = res.root.unwrap();
        let pb = encode_pb_xml(&root);
        let decoded = decode_pb_xml(&pb).unwrap();
        assert_eq!(decoded.name, "root");
        assert_eq!(decoded.namespace_decls.len(), 1);
        let child = decoded.find_child("", "child").unwrap();
        assert_eq!(child.attr_value(SCHEMA_ANDROID, "value"), Some("x"));
        assert_eq!(child.text(), "text");
        assert_eq!(child.line_number, 2);
    }

    #[test]
    fn extract_package() {
        assert_eq!(
            extract_package_from_namespace(SCHEMA_ANDROID),
            Some(ExtractedPackage { package: "android".to_string(), private_namespace: false })
        );
        assert_eq!(
            extract_package_from_namespace(SCHEMA_AUTO),
            Some(ExtractedPackage { package: String::new(), private_namespace: true })
        );
        assert_eq!(extract_package_from_namespace(SCHEMA_TOOLS), None);
    }
}
