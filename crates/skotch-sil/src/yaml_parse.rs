//! Reverse direction: turn the YAML emitted by [`crate::emit_yaml`]
//! back into a [`SilTree`].
//!
//! Hand-rolled for the constrained subset of YAML our emitter
//! produces — no external dependency, predictable behavior, and no
//! surprises about how non-ASCII or escape sequences are handled.
//! The grammar accepted is:
//!
//! ```text
//! document := header_field* ast_field
//! header_field := IDENT ':' VALUE NEWLINE
//! ast_field    := 'ast:' NEWLINE node
//! node         := INDENT? '- '? 'type:' STRING NEWLINE
//!                 (INDENT 'error:'    STRING NEWLINE)?
//!                 (INDENT 'text:'     STRING NEWLINE
//!                 | INDENT 'children:' '[]'? NEWLINE node*)
//! ```
//!
//! Strings are always double-quoted with the escape repertoire the
//! emitter uses: `\"`, `\\`, `\n`, `\r`, `\t`, `\xHH`, and the literal
//! UTF-8 bytes for printable characters. Unrecognized escapes are
//! rejected so we catch parser-emitter divergence at the source.

use crate::tree::{SilData, SilNode, SilTree};
use crate::yaml_kind::syntax_kind_from_name;
use skotch_span::{FileId, Span};

/// Error from parsing YAML back into a SilTree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlParseError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl std::fmt::Display for YamlParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "YAML parse error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for YamlParseError {}

/// Entry point: parse YAML text into a [`SilTree`].
pub fn parse_yaml(input: &str) -> Result<SilTree, YamlParseError> {
    let mut p = Parser::new(input);
    p.parse_document()
}

struct Parser<'a> {
    /// Pre-scanned line views. Each entry is (indent, content_after_indent).
    lines: Vec<(usize, &'a str)>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        let mut lines = Vec::new();
        for raw in input.lines() {
            let indent = raw.chars().take_while(|c| *c == ' ').count();
            let content = &raw[indent..];
            lines.push((indent, content));
        }
        Self { lines, pos: 0 }
    }

    fn err(&self, message: impl Into<String>) -> YamlParseError {
        YamlParseError {
            line: self.pos + 1,
            col: self.lines.get(self.pos).map(|(i, _)| *i + 1).unwrap_or(1),
            message: message.into(),
        }
    }

    fn current(&self) -> Option<(usize, &'a str)> {
        self.lines.get(self.pos).copied()
    }

    fn skip_empty(&mut self) {
        while let Some((_, c)) = self.current() {
            if c.is_empty() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_document(&mut self) -> Result<SilTree, YamlParseError> {
        let mut file = String::new();
        let mut source_length: u32 = 0;
        let mut crlf_normalized = false;

        // Header — every key:value line at indent 0 before `ast:`.
        self.skip_empty();
        loop {
            self.skip_empty();
            let (indent, content) = self.current().ok_or_else(|| self.err("unexpected EOF in header"))?;
            if indent != 0 {
                return Err(self.err(format!(
                    "expected header field at indent 0 (got indent {})",
                    indent
                )));
            }
            if content == "ast:" {
                self.pos += 1;
                break;
            }
            let (key, value) = split_kv(content)
                .ok_or_else(|| self.err(format!("expected `key: value` (got {:?})", content)))?;
            match key {
                "file" => file = parse_string_literal(value, self)?,
                "source_length" => {
                    source_length = value.parse::<u32>().map_err(|_| {
                        self.err(format!("invalid source_length: {:?}", value))
                    })?
                }
                "crlf_normalized" => match value {
                    "true" => crlf_normalized = true,
                    "false" => crlf_normalized = false,
                    _ => {
                        return Err(self.err(format!(
                            "crlf_normalized must be true/false, got {:?}",
                            value
                        )));
                    }
                },
                other => return Err(self.err(format!("unknown header field: {:?}", other))),
            }
            self.pos += 1;
        }

        // ast: — first node lives at indent 2.
        let root = self.parse_node(2, false)?;

        Ok(SilTree {
            file,
            source_length,
            crlf_normalized,
            root,
        })
    }

    /// Parse a node whose `type:` line lives at `expected_indent`.
    ///
    /// When `as_list_item` is true the line opens with `- ` and the
    /// rest of the node's fields live at `expected_indent + 2`.
    fn parse_node(&mut self, expected_indent: usize, as_list_item: bool) -> Result<SilNode, YamlParseError> {
        self.skip_empty();
        let (indent, content) = self
            .current()
            .ok_or_else(|| self.err("unexpected EOF reading node"))?;
        if indent != expected_indent {
            return Err(self.err(format!(
                "expected node at indent {} (got {})",
                expected_indent, indent
            )));
        }
        let (head_content, field_indent) = if as_list_item {
            let rest = content
                .strip_prefix("- ")
                .ok_or_else(|| self.err(format!("expected list item `- `, got {:?}", content)))?;
            (rest, expected_indent + 2)
        } else {
            (content, expected_indent)
        };
        let (key, value) = split_kv(head_content).ok_or_else(|| {
            self.err(format!(
                "expected `type: \"...\"` (got {:?})",
                head_content
            ))
        })?;
        if key != "type" {
            return Err(self.err(format!("expected `type:`, got {:?}", key)));
        }
        let kind_name = parse_string_literal(value, self)?;
        let kind = syntax_kind_from_name(&kind_name);
        self.pos += 1;

        // Optional `error:` line.
        let mut error_msg: Option<String> = None;
        if let Some((i, c)) = self.current() {
            if i == field_indent {
                if let Some((k, v)) = split_kv(c) {
                    if k == "error" {
                        error_msg = Some(parse_string_literal(v, self)?);
                        self.pos += 1;
                    }
                }
            }
        }

        // Either `text:` OR `children:` follows.
        let (indent, content) = self
            .current()
            .ok_or_else(|| self.err("unexpected EOF reading node body"))?;
        if indent != field_indent {
            return Err(self.err(format!(
                "expected field at indent {} (got {})",
                field_indent, indent
            )));
        }
        let (key, value) = split_kv(content).ok_or_else(|| {
            self.err(format!("expected `text:` or `children:` (got {:?})", content))
        })?;
        let (data, span) = match key {
            "text" => {
                let text = parse_string_literal(value, self)?;
                self.pos += 1;
                let data = if let Some(msg) = error_msg {
                    SilData::Error {
                        message: msg,
                        children: vec![SilNode::token(kind, &text, dummy_span())],
                    }
                } else {
                    SilData::Token { text }
                };
                (data, dummy_span())
            }
            "children" => {
                let mut children = Vec::new();
                if value == "[]" {
                    self.pos += 1;
                } else if value.is_empty() {
                    self.pos += 1;
                    // Children list items begin at field_indent + 2.
                    let child_indent = field_indent + 2;
                    loop {
                        self.skip_empty();
                        let Some((i, c)) = self.current() else { break };
                        if i < child_indent {
                            break;
                        }
                        if i > child_indent {
                            return Err(self.err(format!(
                                "child indented {} (expected {})",
                                i, child_indent
                            )));
                        }
                        if !c.starts_with("- ") {
                            break;
                        }
                        let child = self.parse_node(child_indent, true)?;
                        children.push(child);
                    }
                } else {
                    return Err(self.err(format!(
                        "`children:` value must be empty or `[]`, got {:?}",
                        value
                    )));
                }
                let data = if let Some(msg) = error_msg {
                    SilData::Error {
                        message: msg,
                        children,
                    }
                } else {
                    SilData::Composite { children }
                };
                (data, dummy_span())
            }
            other => return Err(self.err(format!("unexpected field {:?}", other))),
        };

        Ok(SilNode { kind, span, data })
    }
}

/// Spans aren't tracked in YAML — set to file 0, 0..0. The
/// reconstruction path doesn't depend on spans, so this is safe.
fn dummy_span() -> Span {
    Span {
        file: FileId(0),
        start: 0,
        end: 0,
    }
}

/// Split `key: value` into `(key, value)`. Returns None when the
/// content does not contain a `:` followed by either end-of-string or
/// a space.
fn split_kv(content: &str) -> Option<(&str, &str)> {
    // Find the first ':' followed by ' ' or end-of-string. The key
    // never contains a colon in our emitted YAML.
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            if i + 1 == bytes.len() {
                return Some((&content[..i], ""));
            } else if bytes[i + 1] == b' ' {
                return Some((&content[..i], &content[i + 2..]));
            }
        }
        i += 1;
    }
    None
}

/// Decode a `"..."` string literal as emitted by the YAML writer.
fn parse_string_literal(value: &str, p: &Parser<'_>) -> Result<String, YamlParseError> {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || *bytes.last().unwrap() != b'"' {
        return Err(p.err(format!("expected double-quoted string, got {:?}", value)));
    }
    let inner = &value[1..value.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let next = chars
            .next()
            .ok_or_else(|| p.err("dangling backslash in string"))?;
        match next {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => {
                let h1 = chars
                    .next()
                    .ok_or_else(|| p.err("short \\xHH escape"))?;
                let h2 = chars
                    .next()
                    .ok_or_else(|| p.err("short \\xHH escape"))?;
                let hex = format!("{}{}", h1, h2);
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| p.err(format!("invalid hex in \\x{}{}", h1, h2)))?;
                out.push(char::from_u32(code).ok_or_else(|| p.err("invalid code point"))?);
            }
            other => {
                return Err(p.err(format!("unknown escape \\{}", other)));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_emit::emit_yaml;
    use skotch_syntax::SyntaxKind;

    #[test]
    fn parse_minimal_tree() {
        let y = "file: \"x.kt\"\nsource_length: 7\ncrlf_normalized: false\nast:\n  type: \"kotlin.FILE\"\n  children:\n    - type: \"PACKAGE_DIRECTIVE\"\n      children:\n        - type: \"package\"\n          text: \"package\"\n";
        let tree = parse_yaml(y).expect("parse");
        assert_eq!(tree.file, "x.kt");
        assert_eq!(tree.source_length, 7);
        assert_eq!(tree.root.kind, SyntaxKind::FILE);
        assert_eq!(tree.reconstruct(), "package");
    }

    #[test]
    fn roundtrip_through_emit_then_parse() {
        use crate::tree::SilNode;
        use skotch_span::{FileId, Span};
        fn s(a: u32, b: u32) -> Span {
            Span {
                file: FileId(0),
                start: a,
                end: b,
            }
        }
        let pkg = SilNode::composite(
            SyntaxKind::PACKAGE_DIRECTIVE,
            vec![
                SilNode::token(SyntaxKind::KW_PACKAGE, "package", s(0, 7)),
                SilNode::token(SyntaxKind::WHITE_SPACE, " ", s(7, 8)),
                SilNode::token(SyntaxKind::IDENTIFIER, "foo", s(8, 11)),
            ],
            s(0, 11),
        );
        let file = SilNode::composite(SyntaxKind::FILE, vec![pkg], s(0, 11));
        let tree = SilTree {
            file: "x.kt".to_string(),
            source_length: 11,
            crlf_normalized: false,
            root: file,
        };
        let yaml = emit_yaml(&tree);
        let reparsed = parse_yaml(&yaml).expect("reparse");
        assert_eq!(reparsed.file, tree.file);
        assert_eq!(reparsed.source_length, tree.source_length);
        assert_eq!(reparsed.reconstruct(), "package foo");
        // Idempotent emit.
        let yaml2 = emit_yaml(&reparsed);
        assert_eq!(yaml, yaml2);
    }

    #[test]
    fn roundtrip_with_escape_chars() {
        use crate::tree::SilNode;
        use skotch_span::{FileId, Span};
        fn s(a: u32, b: u32) -> Span {
            Span {
                file: FileId(0),
                start: a,
                end: b,
            }
        }
        let nl = SilNode::token(SyntaxKind::WHITE_SPACE, "\n\n", s(0, 2));
        let tab = SilNode::token(SyntaxKind::WHITE_SPACE, "\t", s(2, 3));
        let quote = SilNode::token(SyntaxKind::REGULAR_STRING_PART, "a\"b\\c", s(3, 8));
        let file = SilNode::composite(SyntaxKind::FILE, vec![nl, tab, quote], s(0, 8));
        let tree = SilTree {
            file: "x.kt".to_string(),
            source_length: 8,
            crlf_normalized: false,
            root: file,
        };
        let yaml = emit_yaml(&tree);
        let reparsed = parse_yaml(&yaml).expect("reparse");
        assert_eq!(reparsed.reconstruct(), "\n\n\ta\"b\\c");
    }

    #[test]
    fn parse_empty_children_inline() {
        let y = "file: \"x.kt\"\nsource_length: 0\ncrlf_normalized: false\nast:\n  type: \"kotlin.FILE\"\n  children:\n    - type: \"IMPORT_LIST\"\n      children: []\n";
        let tree = parse_yaml(y).expect("parse");
        let SilData::Composite { children } = &tree.root.data else {
            panic!()
        };
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].kind, SyntaxKind::IMPORT_LIST);
        assert!(matches!(&children[0].data, SilData::Composite { children } if children.is_empty()));
    }
}
