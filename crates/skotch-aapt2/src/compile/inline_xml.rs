//! `<aapt:attr>` inline XML extraction.
//!
//! Port of `compile/InlineXmlFormatParser.cpp`: elements of the form
//! `<aapt:attr name="android:drawable"> <inner-xml/> </aapt:attr>`
//! are replaced by an attribute on the parent referencing a synthesized
//! sub-document compiled alongside the main one.

use super::CompiledXml;
use crate::res::table::mangle_entry;
use crate::res::utils::parse_xml_attribute_name;
use crate::res::ResourceFile;
use crate::xml::{
    build_package_namespace, Element, NamespaceDecl, Node, XmlAttribute, SCHEMA_AAPT, SCHEMA_AUTO,
};
use anyhow::{anyhow, bail, Result};

/// Resolves a package alias (XML namespace prefix) against the
/// namespace declarations in scope. Mirrors
/// `xml::PackageAwareVisitor::TransformPackageAlias`.
fn transform_package_alias(
    alias: &str,
    namespace_stack: &[NamespaceDecl],
) -> Option<crate::xml::ExtractedPackage> {
    if alias.is_empty() {
        return Some(crate::xml::ExtractedPackage::default());
    }
    for decl in namespace_stack.iter().rev() {
        if decl.prefix == alias {
            return crate::xml::extract_package_from_namespace(&decl.uri);
        }
    }
    None
}

/// Extracts all `<aapt:attr>` declarations from `root`, returning the
/// synthesized sub-documents. The parent elements gain reference
/// attributes pointing at the new resources.
pub fn extract_inline_xml(root: &mut Element, file: &ResourceFile) -> Result<Vec<CompiledXml>> {
    let mut out = Vec::new();
    let mut counter = 0usize;
    let root_decls = root.namespace_decls.clone();
    let mut namespace_stack = root_decls.clone();
    extract_recursive(root, file, &root_decls, &mut namespace_stack, &mut counter, &mut out)?;
    Ok(out)
}

fn extract_recursive(
    element: &mut Element,
    file: &ResourceFile,
    root_decls: &[NamespaceDecl],
    namespace_stack: &mut Vec<NamespaceDecl>,
    counter: &mut usize,
    out: &mut Vec<CompiledXml>,
) -> Result<()> {
    // Identify <aapt:attr> children of this element.
    let mut extracted: Vec<(usize, XmlAttribute, CompiledXml)> = Vec::new();
    for (index, child) in element.children.iter_mut().enumerate() {
        let Node::Element(child_el) = child else { continue };
        let pushed = child_el.namespace_decls.len();
        namespace_stack.extend(child_el.namespace_decls.iter().cloned());

        if child_el.namespace_uri == SCHEMA_AAPT && child_el.name == "attr" {
            let source = file.source.path.clone();
            let line = child_el.line_number;
            let attr = child_el
                .find_attribute("", "name")
                .ok_or_else(|| anyhow!("{source}:{line}: missing 'name' attribute"))?;

            let reference = parse_xml_attribute_name(&attr.value);
            let name = reference.name.clone().unwrap();
            let package = transform_package_alias(&name.package, namespace_stack)
                .ok_or_else(|| {
                    anyhow!("{source}:{line}: invalid namespace prefix '{}'", name.package)
                })?;
            let private_namespace = package.private_namespace || reference.private_reference;
            let attr_namespace_uri = if name.package.is_empty() {
                String::new()
            } else if package.package.is_empty() {
                SCHEMA_AUTO.to_string()
            } else {
                build_package_namespace(&package.package, private_namespace)
            };

            // Build the sub-document from the single element child.
            let mut new_root: Option<Element> = None;
            for inner in &child_el.children {
                match inner {
                    Node::Text(text) => {
                        if !crate::util::trim_whitespace(&text.text).is_empty() {
                            bail!(
                                "{source}:{}: can't extract text into its own resource",
                                text.line_number
                            );
                        }
                    }
                    Node::Element(inner_el) => {
                        if new_root.is_some() {
                            bail!(
                                "{source}:{}: inline XML resources must have a single root",
                                inner_el.line_number
                            );
                        }
                        let mut cloned = inner_el.clone();
                        // The sub-document inherits the root's namespace
                        // declarations.
                        cloned.namespace_decls = root_decls.to_vec();
                        new_root = Some(cloned);
                    }
                }
            }
            let mut new_root = new_root.ok_or_else(|| {
                anyhow!("{source}:{line}: no inline XML element found in <aapt:attr>")
            })?;

            let mut new_file = file.clone();
            new_file.source.line = Some(line);
            new_file.name.entry =
                mangle_entry("", &format!("{}__{}", file.name.entry, counter));
            new_file.exported_symbols.clear();
            *counter += 1;

            // Recurse: the sub-document may itself contain <aapt:attr>.
            let mut inner_stack = root_decls.to_vec();
            extract_recursive(
                &mut new_root,
                &new_file,
                root_decls,
                &mut inner_stack,
                counter,
                out,
            )?;

            let value = format!("@{}", new_file.name);
            extracted.push((
                index,
                XmlAttribute::new(attr_namespace_uri, name.entry, value),
                CompiledXml { file: new_file, root: new_root },
            ));
        } else {
            extract_recursive(child_el, file, root_decls, namespace_stack, counter, out)?;
        }

        namespace_stack.truncate(namespace_stack.len() - pushed);
    }

    // Replace the <aapt:attr> children with attributes, back to front so
    // indices stay valid.
    for (index, attr, doc) in extracted.into_iter().rev() {
        element.children.remove(index);
        element.attributes.push(attr);
        out.push(doc);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::value::FileType;
    use crate::res::{ResourceName, ResourceType, Source};

    fn file() -> ResourceFile {
        ResourceFile {
            name: ResourceName::new("", ResourceType::Drawable, "icon"),
            source: Source::new("res/drawable/icon.xml"),
            file_type: FileType::ProtoXml,
            ..Default::default()
        }
    }

    #[test]
    fn extracts_inline_drawable() {
        let xml = r#"<animated-selector
    xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt">
  <item android:id="@+id/checked">
    <aapt:attr name="android:drawable">
      <vector android:width="24dp"/>
    </aapt:attr>
  </item>
</animated-selector>"#;
        let parsed = crate::xml::parse_source_xml("res/drawable/icon.xml", xml).unwrap();
        let mut root = parsed.root.unwrap();
        let docs = extract_inline_xml(&mut root, &file()).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file.name.entry, "$icon__0");
        assert_eq!(docs[0].root.name, "vector");

        let item = root.find_child("", "item").unwrap();
        // The aapt:attr child is gone (whitespace text nodes remain),
        // replaced by an attribute.
        assert!(item.child_elements().next().is_none());
        let attr = item
            .find_attribute(crate::xml::SCHEMA_ANDROID, "drawable")
            .expect("drawable attribute added");
        assert_eq!(attr.value, "@drawable/$icon__0");
    }

    #[test]
    fn rejects_multiple_roots() {
        let xml = r#"<root xmlns:aapt="http://schemas.android.com/aapt">
  <aapt:attr name="foo"><a/><b/></aapt:attr>
</root>"#;
        let parsed = crate::xml::parse_source_xml("t.xml", xml).unwrap();
        let mut root = parsed.root.unwrap();
        let err = extract_inline_xml(&mut root, &file()).unwrap_err();
        assert!(err.to_string().contains("single root"), "{err}");
    }
}
