//! YAML emitter — produces the exact byte-identical format the Kotlin
//! script `scripts/kotlin-psi.main.kts` writes.
//!
//! The format is:
//!
//! ```yaml
//! file: "<path>"
//! source_length: <int>
//! crlf_normalized: <true|false>
//! ast:
//!   type: "<SyntaxKind>"
//!   text: "<escaped>"          # leaves only
//!   children:                  # composites only (or `children: []` when empty)
//!     - type: "<SyntaxKind>"
//!       ...
//!   error: "<message>"         # error elements only, before text/children
//! ```
//!
//! Indentation is two spaces per level. Every string is double-quoted
//! with `\xHH` for C0+DEL control characters, `\\` for backslash,
//! `\"` for quote, and `\n`/`\r`/`\t` for the named whitespace
//! escapes. Printable UTF-8 (including non-ASCII) passes through
//! unchanged — YAML 1.2 double-quoted strings preserve it.

use crate::tree::{SilData, SilNode, SilTree};
use crate::yaml_kind::syntax_kind_name;
use std::fmt::Write;

/// Emit the YAML representation of `tree` to a [`String`].
pub fn emit_yaml(tree: &SilTree) -> String {
    let mut out = String::with_capacity(tree.source_length as usize * 4);
    write_header(&mut out, tree);
    out.push_str("ast:\n");
    emit_node(&mut out, &tree.root, "  ", false);
    out
}

fn write_header(out: &mut String, tree: &SilTree) {
    let _ = writeln!(out, "file: {}", quoted(&tree.file));
    let _ = writeln!(out, "source_length: {}", tree.source_length);
    let _ = writeln!(
        out,
        "crlf_normalized: {}",
        if tree.crlf_normalized {
            "true"
        } else {
            "false"
        }
    );
}

/// `indent` is the indent string used at THIS node's nesting depth.
/// `list_item` means we open the first line with `- ` so the node
/// reads as a YAML sequence element.
fn emit_node(out: &mut String, node: &SilNode, indent: &str, list_item: bool) {
    let head = if list_item {
        format!("{}- ", indent)
    } else {
        indent.to_string()
    };
    let field = if list_item {
        format!("{}  ", indent)
    } else {
        indent.to_string()
    };
    let _ = writeln!(out, "{}type: {}", head, quoted(syntax_kind_name(node.kind)));

    if let SilData::Error { message, .. } = &node.data {
        let _ = writeln!(out, "{}error: {}", field, quoted(message));
    }

    match &node.data {
        SilData::Token { text } => {
            let _ = writeln!(out, "{}text: {}", field, quoted(text));
        }
        SilData::Composite { children } | SilData::Error { children, .. } => {
            if children.is_empty() {
                let _ = writeln!(out, "{}children: []", field);
            } else {
                let _ = writeln!(out, "{}children:", field);
                let child_indent = format!("{}  ", field);
                for c in children {
                    emit_node(out, c, &child_indent, true);
                }
            }
        }
    }
}

/// YAML 1.2 double-quoted string escape. The set of escapes matches
/// what the reference Kotlin script does exactly so byte-identical
/// diffs work for hand-curated fixtures.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::SilNode;
    use skotch_span::{FileId, Span};
    use skotch_syntax::SyntaxKind;

    fn s(a: u32, b: u32) -> Span {
        Span {
            file: FileId(0),
            start: a,
            end: b,
        }
    }

    #[test]
    fn header_format_matches_kotlin_script() {
        let root = SilNode::composite(SyntaxKind::FILE, vec![], s(0, 0));
        let tree = SilTree {
            file: "x.kt".to_string(),
            source_length: 42,
            crlf_normalized: false,
            root,
        };
        let yaml = emit_yaml(&tree);
        let head: String = yaml.lines().take(4).collect::<Vec<_>>().join("\n");
        assert_eq!(
            head,
            "file: \"x.kt\"\nsource_length: 42\ncrlf_normalized: false\nast:"
        );
    }

    #[test]
    fn leaf_emits_text_field() {
        let leaf = SilNode::token(SyntaxKind::KW_PACKAGE, "package", s(0, 7));
        let root = SilNode::composite(SyntaxKind::PACKAGE_DIRECTIVE, vec![leaf], s(0, 7));
        let file = SilNode::composite(SyntaxKind::FILE, vec![root], s(0, 7));
        let tree = SilTree {
            file: "x.kt".to_string(),
            source_length: 7,
            crlf_normalized: false,
            root: file,
        };
        let yaml = emit_yaml(&tree);
        // FILE is at indent 2, PACKAGE_DIRECTIVE is a list item at
        // indent 4 (`- ` opens it, so `type:` at col 6), and the
        // leaf nests one more level — `- type:` at col 8, `text:` at
        // col 10. Matches the reference psi.yaml exactly.
        assert!(
            yaml.contains("        - type: \"package\"\n          text: \"package\"\n"),
            "yaml was:\n{}",
            yaml
        );
    }

    #[test]
    fn empty_composite_emits_inline_brackets() {
        let imports = SilNode::composite(SyntaxKind::IMPORT_LIST, vec![], s(0, 0));
        let file = SilNode::composite(SyntaxKind::FILE, vec![imports], s(0, 0));
        let tree = SilTree {
            file: "x.kt".to_string(),
            source_length: 0,
            crlf_normalized: false,
            root: file,
        };
        let yaml = emit_yaml(&tree);
        assert!(
            yaml.contains("- type: \"IMPORT_LIST\"\n      children: []\n"),
            "yaml was: {}",
            yaml
        );
    }

    #[test]
    fn quoted_escapes_match_kotlin_psi_script() {
        assert_eq!(quoted("hi"), "\"hi\"");
        assert_eq!(quoted("\n\n"), "\"\\n\\n\"");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
        assert_eq!(quoted("\t"), "\"\\t\"");
        assert_eq!(quoted("\x01"), "\"\\x01\"");
        assert_eq!(quoted("\x7f"), "\"\\x7f\"");
        assert_eq!(quoted("héllo"), "\"héllo\"");
    }
}
