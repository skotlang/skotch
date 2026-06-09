//! KDoc sub-parser.
//!
//! Given the raw text of a `/** ... */` doc comment, produce a
//! [`SilNode`] tree matching kotlinc's PSI shape for that KDoc:
//!
//! ```text
//! KDoc
//!   KDOC_START "/**"
//!   KDOC_SECTION
//!     [WHITE_SPACE "\n "]
//!     [KDOC_LEADING_ASTERISK "*"]
//!     [KDOC_TEXT " plain text "]
//!     [KDOC_TAG { KDOC_TAG_NAME "@param", " ", KDOC_MARKDOWN_LINK { … }, " ", KDOC_TEXT }]
//!     [KDOC_MARKDOWN_LINK { LBRACKET, KDOC_NAME { … }, RBRACKET, [LBRACKET, KDOC_NAME, RBRACKET]? }]
//!     …
//!   KDOC_END "*/"
//! ```
//!
//! The function is permissive — if a tag name isn't followed by a
//! KDoc-name link, we still emit a KDOC_TAG, just with whatever
//! children we found. The byte-roundtrip invariant is: concatenating
//! every leaf's `text` in pre-order yields the original KDoc bytes.

use crate::tree::SilNode;
use skotch_span::{FileId, Span};
use skotch_syntax::SyntaxKind as S;

/// Build a `KDoc` composite node for the given raw text. `base_start`
/// is the absolute source offset where the `/**` opens.
pub fn parse_kdoc(text: &str, base_start: u32, file_id: FileId) -> SilNode {
    let bytes = text.as_bytes();
    debug_assert!(bytes.starts_with(b"/**"));
    debug_assert!(bytes.ends_with(b"*/"));

    let kdoc_span = |s: usize, e: usize| Span {
        file: file_id,
        start: base_start + s as u32,
        end: base_start + e as u32,
    };

    // `/**` and trailing `*/` bookend the section.
    let mut children: Vec<SilNode> = Vec::with_capacity(8);
    let start_node = SilNode::token(S::KDOC_START, "/**", kdoc_span(0, 3));
    let end_node = SilNode::token(S::KDOC_END, "*/", kdoc_span(text.len() - 2, text.len()));

    let body = &text[3..text.len() - 2];
    let body_offset = 3usize;
    let mut section_kids = parse_section(body, body_offset, base_start, file_id);

    children.push(start_node);

    // Lift leading WHITE_SPACE runs out of KDOC_SECTION — they sit
    // as direct children of KDoc (between KDOC_START and the
    // section). kotlinc puts the `\n ` after `/**` here.
    while section_kids
        .first()
        .map(|n| n.kind == S::WHITE_SPACE)
        .unwrap_or(false)
    {
        children.push(section_kids.remove(0));
    }
    // Same on the tail: trailing WHITE_SPACE runs after the section
    // body lift out to KDoc-level so the `\n ` before `*/` sits next
    // to KDOC_END instead of inside KDOC_SECTION.
    let mut trailing: Vec<SilNode> = Vec::new();
    while section_kids
        .last()
        .map(|n| n.kind == S::WHITE_SPACE)
        .unwrap_or(false)
    {
        trailing.insert(0, section_kids.pop().unwrap());
    }

    if !section_kids.is_empty() {
        let first = section_kids.first().map(|n| n.span.start).unwrap_or(0);
        let last = section_kids.last().map(|n| n.span.end).unwrap_or(0);
        let section = SilNode {
            kind: S::KDOC_SECTION,
            span: Span {
                file: file_id,
                start: first,
                end: last,
            },
            data: crate::tree::SilData::Composite {
                children: section_kids,
            },
        };
        children.push(section);
    }
    children.extend(trailing);
    children.push(end_node);

    SilNode {
        kind: S::KDOC,
        span: kdoc_span(0, text.len()),
        data: crate::tree::SilData::Composite { children },
    }
}

/// Parse the body content between `/**` and `*/` into the children
/// list for a `KDOC_SECTION` composite. The body text is the substring
/// excluding the opening/closing delimiters.
///
/// `body_offset` is where the body starts inside the full KDoc text;
/// `base_start` is the absolute source offset of the KDoc's `/**`.
fn parse_section(body: &str, body_offset: usize, base_start: u32, file_id: FileId) -> Vec<SilNode> {
    let mut out: Vec<SilNode> = Vec::new();
    let bytes = body.as_bytes();
    let mk_span = |s: usize, e: usize| Span {
        file: file_id,
        start: base_start + s as u32,
        end: base_start + e as u32,
    };

    let mut i = 0;
    let mut at_line_start = true;
    // Inside a triple-backtick fenced code block, every line becomes
    // one KDOC_CODE_BLOCK_TEXT (no link/paren parsing applies). The
    // opening fence and closing fence themselves stay as KDOC_TEXT.
    let mut in_code_block = false;
    while i < bytes.len() {
        let b = bytes[i];

        // Multi-line WHITE_SPACE: a run that includes a `\n`. Lookahead
        // to decide — pure inline spaces stay part of KDOC_TEXT.
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            let s = i;
            let mut saw_nl = false;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                if bytes[i] == b'\n' {
                    saw_nl = true;
                }
                i += 1;
            }
            if saw_nl {
                out.push(SilNode::token(
                    S::WHITE_SPACE,
                    &body[s..i],
                    mk_span(body_offset + s, body_offset + i),
                ));
                at_line_start = true;
                continue;
            }
            // No newline — these spaces belong to the surrounding
            // KDOC_TEXT. Roll back so the text loop picks them up.
            i = s;
        }

        // Leading asterisk on a new line: `*` (but not `*/`, that's
        // the end delimiter — handled at the outer level since `body`
        // excludes it).
        if at_line_start && b == b'*' {
            out.push(SilNode::token(
                S::KDOC_LEADING_ASTERISK,
                "*",
                mk_span(body_offset + i, body_offset + i + 1),
            ));
            i += 1;
            at_line_start = false;
            continue;
        }
        at_line_start = false;

        // Inside a code block, consume the rest of the line. If the
        // line contains a closing ` ``` ` fence, the whole line goes
        // out as one KDOC_TEXT (kotlinc's shape for the fence line).
        // Otherwise the whole line content is one KDOC_CODE_BLOCK_TEXT.
        if in_code_block {
            let line_start = i;
            let mut line_end = i;
            let mut close_fence_at: Option<usize> = None;
            while line_end < bytes.len() && bytes[line_end] != b'\n' && bytes[line_end] != b'\r' {
                if line_end + 2 < bytes.len()
                    && bytes[line_end] == b'`'
                    && bytes[line_end + 1] == b'`'
                    && bytes[line_end + 2] == b'`'
                {
                    close_fence_at = Some(line_end);
                    line_end += 3;
                    continue;
                }
                line_end += 1;
            }
            if line_end > line_start {
                if close_fence_at.is_some() {
                    out.push(SilNode::token(
                        S::KDOC_TEXT,
                        &body[line_start..line_end],
                        mk_span(body_offset + line_start, body_offset + line_end),
                    ));
                    in_code_block = false;
                } else {
                    out.push(SilNode::token(
                        S::KDOC_CODE_BLOCK_TEXT,
                        &body[line_start..line_end],
                        mk_span(body_offset + line_start, body_offset + line_end),
                    ));
                }
            }
            i = line_end;
            continue;
        }

        // KDOC_TAG: `@ident ...`
        if b == b'@' {
            let (tag, consumed) = parse_tag(&body[i..], body_offset + i, base_start, file_id);
            out.push(tag);
            i += consumed;
            // parse_tag stops at the next `@`-tag start, which means
            // i now sits at a leading `*` of a new line. Mark
            // at_line_start so the asterisk is emitted as
            // KDOC_LEADING_ASTERISK rather than absorbed as text.
            at_line_start = true;
            continue;
        }

        // KDOC_MARKDOWN_LINK: `[name]` or `[name][target]`.
        if b == b'[' {
            if let Some((link, consumed)) =
                parse_markdown_link(&body[i..], body_offset + i, base_start, file_id)
            {
                out.push(link);
                i += consumed;
                continue;
            }
            // fall through: treat as plain text
        }

        // KDOC_LPAR / KDOC_RPAR: kotlinc treats `(` and `)` as standalone
        // structural tokens inside a section so that descriptions like
        // `Reports a summary (count per rule) of the ...` split into
        // text / `(` / text / `)` / text rather than one long run.
        if b == b'(' {
            out.push(SilNode::token(
                S::KDOC_LPAR,
                "(",
                mk_span(body_offset + i, body_offset + i + 1),
            ));
            i += 1;
            continue;
        }
        if b == b')' {
            out.push(SilNode::token(
                S::KDOC_RPAR,
                ")",
                mk_span(body_offset + i, body_offset + i + 1),
            ));
            i += 1;
            continue;
        }

        // KDOC_TEXT: run until a structural boundary. Stops at `\n`
        // (handled by the WS arm above), `@` (tag), `[` (potential
        // link), or `(`/`)` (paren tokens, handled above). Pure inline
        // whitespace does NOT terminate the text run.
        //
        // Special cases:
        //   * `[display][target]` — kotlinc treats the `[display]`
        //     part as plain KDOC_TEXT (display brackets are literal
        //     text in the doc) and only the `[target]` becomes a
        //     KDOC_MARKDOWN_LINK. We absorb `[display]` into the
        //     running text and stop at `[target]`.
        //   * `[non-identifier]` — `[` content that isn't a valid
        //     KDOC_NAME (e.g. `[Foo<*>]`, `[a + b]`) is not a link;
        //     consume the `[...]` as plain text.
        let text_start = i;
        while i < bytes.len() {
            let bb = bytes[i];
            if matches!(bb, b'\n' | b'\r' | b'@' | b'(' | b')') {
                break;
            }
            if bb == b'[' {
                if let Some((display_end, has_target)) = peek_display_target(&body[i..]) {
                    if has_target {
                        // `[display][target]` — kotlinc absorbs the
                        // first `[display]` into the running text
                        // regardless of validity; the next loop turn
                        // will pick up `[target]` and emit it as the
                        // KDOC_MARKDOWN_LINK.
                        i += display_end;
                        continue;
                    }
                    // Single `[...]`: only treat as a link boundary
                    // when the inner content forms a valid KDOC_NAME.
                    let inner = &body[i + 1..i + display_end - 1];
                    if is_valid_kdoc_name(inner) {
                        break;
                    }
                    // Otherwise absorb `[...]` as plain text.
                    i += display_end;
                    continue;
                }
                break;
            }
            i += 1;
        }
        // Triple-backtick fence inside the text run opens a code
        // block. Emit the text-so-far AND the fence as one KDOC_TEXT
        // (kotlinc keeps the opening fence as part of the surrounding
        // text), then flip into code-block mode.
        let fence_at = body[text_start..i].rfind("```");
        if let Some(off) = fence_at {
            if text_start + off + 3 == i
                || i == bytes.len()
                || matches!(bytes.get(i), Some(b'\n') | Some(b'\r'))
            {
                out.push(SilNode::token(
                    S::KDOC_TEXT,
                    &body[text_start..i],
                    mk_span(body_offset + text_start, body_offset + i),
                ));
                in_code_block = true;
                continue;
            }
        }
        if i > text_start {
            out.push(SilNode::token(
                S::KDOC_TEXT,
                &body[text_start..i],
                mk_span(body_offset + text_start, body_offset + i),
            ));
            continue;
        }
        // No structural arm matched and the text loop made no
        // progress — consume the byte as a single-char text run to
        // guarantee forward progress (defends against infinite
        // loops on malformed input).
        out.push(SilNode::token(
            S::KDOC_TEXT,
            &body[i..i + 1],
            mk_span(body_offset + i, body_offset + i + 1),
        ));
        i += 1;
    }
    out
}

/// Inspect a `[...]` starting at offset 0 of `s`. Returns
/// `Some((end_of_first_bracket, has_target))` if a `[...]` is found
/// on the same line; `has_target` is true iff `[`...`]` is
/// immediately followed by another `[`. Used to decide whether a
/// bracket starts a display-brackets prefix or a real markdown link.
fn peek_display_target(s: &str) -> Option<(usize, bool)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let close_rel = bytes[1..].iter().position(|&b| b == b']' || b == b'\n')?;
    if bytes[1 + close_rel] != b']' {
        return None;
    }
    let close = 1 + close_rel; // index of ']' in s
    let after = close + 1;
    let has_target = bytes.get(after) == Some(&b'[');
    Some((after, has_target))
}

/// Parse `@ident <name>? <text>?` into a KDOC_TAG composite. Returns
/// the composite and how many bytes of `s` were consumed.
fn parse_tag(s: &str, abs_start: usize, base_start: u32, file_id: FileId) -> (SilNode, usize) {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes[0], b'@');
    let mk_span = |start: usize, end: usize| Span {
        file: file_id,
        start: base_start + start as u32,
        end: base_start + end as u32,
    };
    let mut children: Vec<SilNode> = Vec::new();

    // Tag name: `@` followed by an ident.
    let mut i = 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let name_text = &s[..i];
    children.push(SilNode::token(
        S::KDOC_TAG_NAME,
        name_text,
        mk_span(abs_start, abs_start + i),
    ));

    // Optional " " then a `KDOC_MARKDOWN_LINK` (the parameter name).
    if i < bytes.len() && bytes[i] == b' ' {
        let ws_start = i;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        children.push(SilNode::token(
            S::WHITE_SPACE,
            &s[ws_start..i],
            mk_span(abs_start + ws_start, abs_start + i),
        ));

        // The first thing after the tag name is either a markdown
        // link `[ident]` or a bare identifier wrapped as KDOC_NAME →
        // KDOC_MARKDOWN_LINK with no brackets. kotlinc treats both
        // shapes uniformly here.
        if i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            // Bare identifier: kotlinc wraps it as KDOC_MARKDOWN_LINK
            // → KDOC_NAME → IDENTIFIER (no brackets).
            let id_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let id_text = &s[id_start..i];
            let id_node = SilNode::token(
                S::IDENTIFIER,
                id_text,
                mk_span(abs_start + id_start, abs_start + i),
            );
            let name_node = SilNode {
                kind: S::KDOC_NAME,
                span: id_node.span,
                data: crate::tree::SilData::Composite {
                    children: vec![id_node],
                },
            };
            let link_node = SilNode {
                kind: S::KDOC_MARKDOWN_LINK,
                span: name_node.span,
                data: crate::tree::SilData::Composite {
                    children: vec![name_node],
                },
            };
            children.push(link_node);
        } else if i < bytes.len() && bytes[i] == b'[' {
            if let Some((link, consumed)) =
                parse_markdown_link(&s[i..], abs_start + i, base_start, file_id)
            {
                children.push(link);
                i += consumed;
            }
        }
    }

    // Optional second whitespace + description that spans the
    // remaining lines until the next `@`-tag or the end of the KDoc.
    if i < bytes.len() && bytes[i] == b' ' {
        let ws_start = i;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        children.push(SilNode::token(
            S::WHITE_SPACE,
            &s[ws_start..i],
            mk_span(abs_start + ws_start, abs_start + i),
        ));
    }
    // Multi-line description: consume bytes until we hit either
    //   (a) end of the input (the `*/` was stripped from `body` by
    //       the caller, so end-of-`s` == end of KDoc), or
    //   (b) a new `@`-tag at the start of a fresh line (after the
    //       leading `*` and optional whitespace).
    let mut at_line_start = false;
    let mut text_start = i;
    let flush_text = |children: &mut Vec<SilNode>, lo: usize, hi: usize| {
        if hi > lo {
            children.push(SilNode::token(
                S::KDOC_TEXT,
                &s[lo..hi],
                mk_span(abs_start + lo, abs_start + hi),
            ));
        }
    };
    while i < bytes.len() {
        let bb = bytes[i];

        if matches!(bb, b'\n' | b' ' | b'\t' | b'\r') && contains_newline_run(&bytes[i..]) {
            // Multi-line WS run. Before consuming, peek to see if it
            // ends at a new `@`-tag OR at end-of-body (the closing
            // `*/` was stripped, so end-of-bytes means we're at the
            // KDoc terminator). In both cases, the WS belongs to the
            // outer SECTION, not to this tag.
            let mut probe = i;
            while probe < bytes.len() && matches!(bytes[probe], b' ' | b'\t' | b'\r' | b'\n') {
                probe += 1;
            }
            let ends_at_eof = probe >= bytes.len();
            let ends_at_new_tag = bytes.get(probe) == Some(&b'*') && {
                let mut after = probe + 1;
                while after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
                    after += 1;
                }
                bytes.get(after) == Some(&b'@')
            };
            if ends_at_eof || ends_at_new_tag {
                flush_text(&mut children, text_start, i);
                text_start = i;
                break;
            }
            // Otherwise the WS is part of this tag's continuation.
            flush_text(&mut children, text_start, i);
            let ws_start = i;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            children.push(SilNode::token(
                S::WHITE_SPACE,
                &s[ws_start..i],
                mk_span(abs_start + ws_start, abs_start + i),
            ));
            at_line_start = true;
            text_start = i;
            continue;
        }

        if at_line_start && bb == b'*' {
            // Asterisk at line start — could be a continuation line.
            // Look at what follows after `*` (and any space) to
            // decide if this is a new `@`-tag.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'@') {
                // New tag starts — stop this tag here.
                break;
            }
            // Continuation: emit the asterisk and continue.
            flush_text(&mut children, text_start, i);
            children.push(SilNode::token(
                S::KDOC_LEADING_ASTERISK,
                "*",
                mk_span(abs_start + i, abs_start + i + 1),
            ));
            i += 1;
            text_start = i;
            at_line_start = false;
            continue;
        }
        at_line_start = false;

        // Markdown link inside the tag description.
        if bb == b'[' {
            if let Some((display_end, has_target)) = peek_display_target(&s[i..]) {
                if has_target {
                    // Include `[display]` in the running text.
                    i += display_end;
                    continue;
                }
            }
            flush_text(&mut children, text_start, i);
            if let Some((link, consumed)) =
                parse_markdown_link(&s[i..], abs_start + i, base_start, file_id)
            {
                children.push(link);
                i += consumed;
                text_start = i;
                continue;
            }
        }

        // Paren tokens inside the tag description split runs into
        // KDOC_TEXT / KDOC_LPAR / KDOC_TEXT / KDOC_RPAR / ...
        if bb == b'(' || bb == b')' {
            flush_text(&mut children, text_start, i);
            let kind = if bb == b'(' {
                S::KDOC_LPAR
            } else {
                S::KDOC_RPAR
            };
            let ch = if bb == b'(' { "(" } else { ")" };
            children.push(SilNode::token(
                kind,
                ch,
                mk_span(abs_start + i, abs_start + i + 1),
            ));
            i += 1;
            text_start = i;
            continue;
        }

        i += 1;
    }
    flush_text(&mut children, text_start, i);

    let span = Span {
        file: file_id,
        start: base_start + abs_start as u32,
        end: base_start + (abs_start + i) as u32,
    };
    let tag = SilNode {
        kind: S::KDOC_TAG,
        span,
        data: crate::tree::SilData::Composite { children },
    };
    (tag, i)
}

/// Does the immediately-following whitespace run contain `\n`?
fn contains_newline_run(bytes: &[u8]) -> bool {
    for &b in bytes {
        match b {
            b'\n' => return true,
            b' ' | b'\t' | b'\r' => continue,
            _ => return false,
        }
    }
    false
}

/// Parse `[name]` or `[name][target]` into a `KDOC_MARKDOWN_LINK`
/// composite. Returns `None` if the input doesn't start with `[` or
/// no matching `]` is found on the same line.
fn parse_markdown_link(
    s: &str,
    abs_start: usize,
    base_start: u32,
    file_id: FileId,
) -> Option<(SilNode, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mk_span = |start: usize, end: usize| Span {
        file: file_id,
        start: base_start + start as u32,
        end: base_start + end as u32,
    };

    let close = bytes.iter().position(|&b| b == b']' || b == b'\n')?;
    if bytes[close] != b']' {
        return None;
    }
    let name_text = &s[1..close];
    // kotlinc only treats `[...]` as a markdown link when the content
    // is a valid (possibly dotted) Kotlin identifier — `[Foo]`,
    // `[Foo.bar]`. Anything else (e.g. `[Foo<*>]`, `[a + b]`) stays
    // as plain KDOC_TEXT.
    if !is_valid_kdoc_name(name_text) {
        return None;
    }
    let name_node = build_kdoc_name(name_text, abs_start + 1, base_start, file_id);

    let mut children: Vec<SilNode> = vec![
        SilNode::token(S::LBRACKET, "[", mk_span(abs_start, abs_start + 1)),
        name_node,
        SilNode::token(
            S::RBRACKET,
            "]",
            mk_span(abs_start + close, abs_start + close + 1),
        ),
    ];

    let mut i = close + 1;
    // Optional `[target]`.
    if bytes.get(i) == Some(&b'[') {
        let inner_start = i + 1;
        if let Some(rel_close) = bytes[inner_start..]
            .iter()
            .position(|&b| b == b']' || b == b'\n')
        {
            if bytes[inner_start + rel_close] == b']' {
                let tgt_text = &s[inner_start..inner_start + rel_close];
                let tgt_node =
                    build_kdoc_name(tgt_text, abs_start + inner_start, base_start, file_id);
                children.push(SilNode::token(
                    S::LBRACKET,
                    "[",
                    mk_span(abs_start + i, abs_start + i + 1),
                ));
                children.push(tgt_node);
                children.push(SilNode::token(
                    S::RBRACKET,
                    "]",
                    mk_span(
                        abs_start + inner_start + rel_close,
                        abs_start + inner_start + rel_close + 1,
                    ),
                ));
                i = inner_start + rel_close + 1;
            }
        }
    }

    let span = Span {
        file: file_id,
        start: base_start + abs_start as u32,
        end: base_start + (abs_start + i) as u32,
    };
    let link = SilNode {
        kind: S::KDOC_MARKDOWN_LINK,
        span,
        data: crate::tree::SilData::Composite { children },
    };
    Some((link, i))
}

/// `true` when `text` is a (possibly dotted) Kotlin identifier
/// — `Foo`, `Foo.bar`, `Foo.bar.baz`. Names with operators, generics,
/// or whitespace are NOT valid KDOC names and should be left as plain
/// text by the caller.
fn is_valid_kdoc_name(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let mut expect_ident_start = true;
    for &b in text.as_bytes() {
        if b == b'.' {
            if expect_ident_start {
                return false; // leading dot or consecutive dots
            }
            expect_ident_start = true;
            continue;
        }
        let is_start_char = b.is_ascii_alphabetic() || b == b'_';
        let is_cont_char = b.is_ascii_alphanumeric() || b == b'_';
        if expect_ident_start {
            if !is_start_char {
                return false;
            }
            expect_ident_start = false;
        } else if !is_cont_char {
            return false;
        }
    }
    !expect_ident_start
}

/// Build a `KDOC_NAME` composite from a dotted name like `Foo.bar.baz`.
/// Each segment is an `IDENTIFIER` and dots become `DOT` tokens; for
/// multi-segment names, the structure is left-recursive — every dot
/// wraps the prior `KDOC_NAME` in a new `KDOC_NAME` parent.
fn build_kdoc_name(text: &str, abs_start: usize, base_start: u32, file_id: FileId) -> SilNode {
    let mk_span = |s: usize, e: usize| Span {
        file: file_id,
        start: base_start + s as u32,
        end: base_start + e as u32,
    };

    // Split on `.` and build left-recursive KDOC_NAME chain.
    let mut parts: Vec<(usize, usize, &str)> = Vec::new();
    let mut seg_start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch == '.' {
            parts.push((seg_start, i, &text[seg_start..i]));
            seg_start = i + 1;
        }
    }
    parts.push((seg_start, text.len(), &text[seg_start..]));

    if parts.len() == 1 {
        let (s, e, t) = parts[0];
        let ident = SilNode::token(S::IDENTIFIER, t, mk_span(abs_start + s, abs_start + e));
        return SilNode {
            kind: S::KDOC_NAME,
            span: ident.span,
            data: crate::tree::SilData::Composite {
                children: vec![ident],
            },
        };
    }

    // Multi-segment: left-recursive nesting.
    let (s0, e0, t0) = parts[0];
    let first_ident = SilNode::token(S::IDENTIFIER, t0, mk_span(abs_start + s0, abs_start + e0));
    let mut current = SilNode {
        kind: S::KDOC_NAME,
        span: first_ident.span,
        data: crate::tree::SilData::Composite {
            children: vec![first_ident],
        },
    };

    for &(s, e, t) in &parts[1..] {
        // The `.` before this segment.
        let dot_pos = s - 1;
        let dot = SilNode::token(
            S::DOT,
            ".",
            mk_span(abs_start + dot_pos, abs_start + dot_pos + 1),
        );
        let ident = SilNode::token(S::IDENTIFIER, t, mk_span(abs_start + s, abs_start + e));
        let span_start = current.span.start;
        let span_end = ident.span.end;
        let new_name = SilNode {
            kind: S::KDOC_NAME,
            span: Span {
                file: file_id,
                start: span_start,
                end: span_end,
            },
            data: crate::tree::SilData::Composite {
                children: vec![current, dot, ident],
            },
        };
        current = new_name;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::SilData;
    use skotch_span::FileId;

    fn leaves_text(node: &SilNode) -> String {
        let mut s = String::new();
        node.collect_text(&mut s);
        s
    }

    #[test]
    fn parse_simple_one_line_kdoc() {
        let txt = "/** body */";
        let node = parse_kdoc(txt, 0, FileId(0));
        assert_eq!(leaves_text(&node), txt);
        assert_eq!(node.kind, S::KDOC);
        let SilData::Composite { children } = &node.data else {
            panic!()
        };
        assert_eq!(children[0].kind, S::KDOC_START);
        assert_eq!(children[1].kind, S::KDOC_SECTION);
        assert_eq!(children[2].kind, S::KDOC_END);
    }

    #[test]
    fn parse_multiline_kdoc_with_asterisks_and_text() {
        let txt = "/**\n * line1\n * line2\n */";
        let node = parse_kdoc(txt, 0, FileId(0));
        assert_eq!(leaves_text(&node), txt);
    }

    #[test]
    fn parse_kdoc_with_param_tag() {
        let txt = "/**\n * @param argv The command line\n */";
        let node = parse_kdoc(txt, 0, FileId(0));
        assert_eq!(leaves_text(&node), txt);
    }

    #[test]
    fn parse_kdoc_with_markdown_link() {
        let txt = "/** see [Foo] */";
        let node = parse_kdoc(txt, 0, FileId(0));
        assert_eq!(leaves_text(&node), txt);
    }

    #[test]
    fn parse_kdoc_with_dotted_name_link() {
        let txt = "/** see [echo][CliktCommand.echo] */";
        let node = parse_kdoc(txt, 0, FileId(0));
        assert_eq!(leaves_text(&node), txt);
    }
}
