//! `dump xmltree`: port of `Debug::DumpXml` (the `XmlPrinter` visitor).

use super::printer::Printer;
use super::values::pretty_print_item;
use crate::res::ResourceId;
use crate::xml::{Element, Node};

fn normalize_for_output(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

fn visit_element(el: &Element, in_scope: &mut Vec<(String, String)>, printer: &mut Printer) {
    // The binary-XML parser attaches every namespace that is still in
    // scope to each element; aapt2 prints a declaration only on the
    // element that introduced it, so skip declarations an ancestor
    // already carries.
    let new_decls: Vec<_> = el
        .namespace_decls
        .iter()
        .filter(|d| !in_scope.iter().any(|(p, u)| *p == d.prefix && *u == d.uri))
        .collect();
    for decl in &new_decls {
        printer.println(format!(
            "N: {}={} (line={})",
            decl.prefix, decl.uri, decl.line_number
        ));
        printer.indent();
        in_scope.push((decl.prefix.clone(), decl.uri.clone()));
    }

    printer.print("E: ");
    if !el.namespace_uri.is_empty() {
        printer.print(&el.namespace_uri);
        printer.print(":");
    }
    printer.println(format!("{} (line={})", el.name, el.line_number));
    printer.indent();

    for attr in &el.attributes {
        printer.print("A: ");
        if !attr.namespace_uri.is_empty() {
            printer.print(&attr.namespace_uri);
            printer.print(":");
        }
        printer.print(&attr.name);
        if let Some(compiled_attribute) = &attr.compiled_attribute {
            printer.print("(");
            printer.print(compiled_attribute.id.unwrap_or(ResourceId(0)).to_string());
            printer.print(")");
        }
        printer.print("=");
        if let Some(compiled_value) = &attr.compiled_value {
            pretty_print_item(compiled_value, printer);
        } else {
            printer.print("\"");
            printer.print(&attr.value);
            printer.print("\"");
        }
        if !attr.value.is_empty() {
            printer.print(" (Raw: \"");
            printer.print(&attr.value);
            printer.print("\")");
        }
        printer.println_empty();
    }

    printer.indent();
    for child in &el.children {
        match child {
            Node::Element(child_el) => visit_element(child_el, in_scope, printer),
            Node::Text(text) => {
                printer.println(format!("T: '{}'", normalize_for_output(&text.text)));
            }
        }
    }
    printer.undent();
    printer.undent();

    for _ in &new_decls {
        in_scope.pop();
        printer.undent();
    }
}

/// Port of `Debug::DumpXml`.
pub fn dump_xml(root: &Element, printer: &mut Printer) {
    visit_element(root, &mut Vec::new(), printer);
}
