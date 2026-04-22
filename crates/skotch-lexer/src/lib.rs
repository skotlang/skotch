//! Hand-rolled lexer for the subset of Kotlin 2 we accept in PR #1.
//!
//! Hand-rolled because Kotlin's string templates require mode switching
//! mid-stream (`"hello $name"`, `"value: ${expr}"`), which is awkward in
//! `logos`. The cost is ~250 lines that we own outright.
//!
//! ## Output shape
//!
//! Returns a [`LexedFile`] containing:
//!
//! - a flat `Vec<Token>` (kind + span only)
//! - a parallel `Vec<Option<TokenPayload>>` carrying decoded literal
//!   values (interpolation-decoded string chunks, parsed integers, raw
//!   identifier text). The parser interns identifiers itself so the
//!   lexer needs no interner reference.
//!
//! ## String templates
//!
//! Every string literal — whether templated or not — is emitted as the
//! sequence:
//!
//! ```text
//! StringStart, (StringChunk | StringIdentRef | StringExprStart … StringExprEnd)*, StringEnd
//! ```
//!
//! For an interpolated `"hello $name"` we emit:
//!
//! ```text
//! StringStart "hello "  StringChunk("hello ")  StringIdentRef("name")  StringEnd
//! ```
//!
//! For `"value: ${1 + 2}"`:
//!
//! ```text
//! StringStart  StringChunk("value: ")  StringExprStart  IntLit("1")  Plus  IntLit("2")  StringExprEnd  StringEnd
//! ```
//!
//! Inside a `${ … }` block we re-enter normal lexer mode and track brace
//! depth so nested `{` `}` don't prematurely terminate the interpolation.
//!
//! ## Newlines
//!
//! Kotlin treats newlines as soft statement terminators. The lexer
//! preserves them as `Newline` tokens; the parser decides whether to
//! consume them based on context. Consecutive newlines collapse into a
//! single token.

use skotch_diagnostics::{Diagnostic, Diagnostics};
use skotch_span::{FileId, Span};
use skotch_syntax::{Token, TokenKind};

/// Decoded payload for tokens that carry per-instance data.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenPayload {
    /// Source text of an identifier. The parser interns it.
    Ident(String),
    /// Parsed integer value. PR #1 only supports decimal `i64`.
    Int(i64),
    /// Decoded string chunk content (escapes resolved).
    StringChunk(String),
    /// Parsed floating-point value.
    Double(f64),
    /// Identifier referenced by a `$ident` interpolation.
    StringIdentRef(String),
}

/// The full output of lexing one source file.
#[derive(Clone, Debug)]
pub struct LexedFile {
    pub file: FileId,
    pub tokens: Vec<Token>,
    pub payloads: Vec<Option<TokenPayload>>,
}

impl LexedFile {
    pub fn payload(&self, idx: usize) -> Option<&TokenPayload> {
        self.payloads.get(idx).and_then(|p| p.as_ref())
    }
}

/// Lex `source` belonging to `file`. Errors are pushed into `diags` and
/// the lexer attempts to continue, marking failed runs with `Error`
/// tokens. The parser stops at the first `Error` it sees.
pub fn lex(file: FileId, source: &str, diags: &mut Diagnostics) -> LexedFile {
    let mut lx = Lexer {
        file,
        bytes: source.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        payloads: Vec::new(),
        diags,
    };
    lx.run();
    LexedFile {
        file,
        tokens: lx.tokens,
        payloads: lx.payloads,
    }
}

struct Lexer<'a> {
    file: FileId,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    payloads: Vec<Option<TokenPayload>>,
    diags: &'a mut Diagnostics,
}

impl<'a> Lexer<'a> {
    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            self.next_token();
        }
        self.emit(TokenKind::Eof, self.pos, self.pos, None);
    }

    /// Lex one token. May recurse into [`scan_string`] which itself
    /// re-enters this state machine for `${ … }` interpolations.
    fn next_token(&mut self) {
        // Skip plain whitespace (not newlines).
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let start = self.pos;
        let Some(b) = self.peek() else { return };

        // Newlines: collapse a run of `\n`s into one `Newline` token.
        if b == b'\n' {
            while let Some(b'\n') = self.peek() {
                self.pos += 1;
            }
            self.emit(TokenKind::Newline, start, self.pos, None);
            return;
        }

        // Line comment: `// ... \n`. Consume to end of line; produce no
        // token (comments are pure trivia for the parser's purposes).
        if b == b'/' && self.peek_at(1) == Some(b'/') {
            while let Some(b) = self.peek() {
                if b == b'\n' {
                    break;
                }
                self.pos += 1;
            }
            return;
        }

        // Block comment: `/* ... */`. Not nested-aware in PR #1.
        if b == b'/' && self.peek_at(1) == Some(b'*') {
            self.pos += 2;
            while self.pos < self.bytes.len() {
                if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                    self.pos += 2;
                    return;
                }
                self.pos += 1;
            }
            self.error(start, self.pos, "unterminated block comment");
            return;
        }

        // Identifiers and keywords: `[A-Za-z_][A-Za-z0-9_]*`.
        if b.is_ascii_alphabetic() || b == b'_' {
            self.scan_ident();
            return;
        }

        // Integer literals: `[0-9]+`. PR #1 doesn't handle hex / float.
        if b.is_ascii_digit() {
            self.scan_int();
            return;
        }

        // String literals (always templated form).
        if b == b'"' {
            self.scan_string();
            return;
        }

        // Char literals: 'X' or '\n' etc. Emitted as CharLit with the code point.
        if b == b'\'' {
            self.pos += 1; // skip opening quote
            let ch_val = if self.pos < self.bytes.len() && self.bytes[self.pos] == b'\\' {
                // Escape sequence
                self.pos += 1;
                match self.peek() {
                    Some(b'n') => {
                        self.pos += 1;
                        b'\n'
                    }
                    Some(b't') => {
                        self.pos += 1;
                        b'\t'
                    }
                    Some(b'r') => {
                        self.pos += 1;
                        b'\r'
                    }
                    Some(b'\\') => {
                        self.pos += 1;
                        b'\\'
                    }
                    Some(b'\'') => {
                        self.pos += 1;
                        b'\''
                    }
                    Some(b'0') => {
                        self.pos += 1;
                        0u8
                    }
                    _ => {
                        self.pos += 1;
                        b'?'
                    }
                }
            } else if self.pos < self.bytes.len() {
                let c = self.bytes[self.pos];
                self.pos += 1;
                c
            } else {
                0u8
            };
            // Consume closing quote
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'\'' {
                self.pos += 1;
            }
            self.emit(
                TokenKind::CharLit,
                start,
                self.pos,
                Some(TokenPayload::Int(ch_val as i64)),
            );
            return;
        }

        // Punctuation. Multi-character forms first.
        let two = (b, self.peek_at(1));
        let kind = match two {
            (b'-', Some(b'>')) => Some((TokenKind::Arrow, 2)),
            (b'=', Some(b'=')) => Some((TokenKind::EqEq, 2)),
            (b'!', Some(b'=')) => Some((TokenKind::NotEq, 2)),
            (b'<', Some(b'=')) => Some((TokenKind::LtEq, 2)),
            (b'>', Some(b'=')) => Some((TokenKind::GtEq, 2)),
            (b'&', Some(b'&')) => Some((TokenKind::AmpAmp, 2)),
            (b'|', Some(b'|')) => Some((TokenKind::PipePipe, 2)),
            (b':', Some(b':')) => Some((TokenKind::ColonColon, 2)),
            (b'.', Some(b'.')) => Some((TokenKind::DotDot, 2)),
            (b'+', Some(b'=')) => Some((TokenKind::PlusEq, 2)),
            (b'-', Some(b'=')) => Some((TokenKind::MinusEq, 2)),
            (b'*', Some(b'=')) => Some((TokenKind::StarEq, 2)),
            (b'/', Some(b'=')) => Some((TokenKind::SlashEq, 2)),
            (b'%', Some(b'=')) => Some((TokenKind::PercentEq, 2)),
            (b'?', Some(b'.')) => Some((TokenKind::QuestionDot, 2)),
            (b'?', Some(b':')) => Some((TokenKind::Elvis, 2)),
            (b'!', Some(b'!')) => Some((TokenKind::BangBang, 2)),
            (b'+', Some(b'+')) => Some((TokenKind::PlusPlus, 2)),
            (b'-', Some(b'-')) => Some((TokenKind::MinusMinus, 2)),
            _ => None,
        };
        if let Some((k, n)) = kind {
            self.pos += n;
            self.emit(k, start, self.pos, None);
            return;
        }

        let single = match b {
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b'[' => Some(TokenKind::LBracket),
            b']' => Some(TokenKind::RBracket),
            b',' => Some(TokenKind::Comma),
            b';' => Some(TokenKind::Semi),
            b':' => Some(TokenKind::Colon),
            b'.' => Some(TokenKind::Dot),
            b'=' => Some(TokenKind::Eq),
            b'+' => Some(TokenKind::Plus),
            b'-' => Some(TokenKind::Minus),
            b'*' => Some(TokenKind::Star),
            b'/' => Some(TokenKind::Slash),
            b'%' => Some(TokenKind::Percent),
            b'!' => Some(TokenKind::Bang),
            b'?' => Some(TokenKind::Question),
            b'<' => Some(TokenKind::Lt),
            b'>' => Some(TokenKind::Gt),
            b'@' => Some(TokenKind::At),
            _ => None,
        };
        if let Some(k) = single {
            self.pos += 1;
            self.emit(k, start, self.pos, None);
            return;
        }

        // Unknown byte: emit Error and advance to avoid an infinite loop.
        self.pos += 1;
        self.error(
            start,
            self.pos,
            format!("unexpected character {:?}", b as char),
        );
    }

    fn scan_ident(&mut self) {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("ASCII identifier")
            .to_string();
        let kind = keyword_kind(&text).unwrap_or(TokenKind::Ident);
        let payload = if kind == TokenKind::Ident {
            Some(TokenPayload::Ident(text))
        } else {
            None
        };
        self.emit(kind, start, self.pos, payload);
    }

    fn scan_int(&mut self) {
        let start = self.pos;
        // Check for hex prefix 0x / 0X
        let is_hex = self.pos + 1 < self.bytes.len()
            && self.bytes[self.pos] == b'0'
            && (self.bytes[self.pos + 1] == b'x' || self.bytes[self.pos + 1] == b'X');
        if is_hex {
            self.pos += 2; // skip "0x"
            while let Some(b) = self.peek() {
                if b.is_ascii_hexdigit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        } else {
            // Check for binary prefix 0b / 0B
            let is_bin = self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'0'
                && (self.bytes[self.pos + 1] == b'b' || self.bytes[self.pos + 1] == b'B');
            if is_bin {
                self.pos += 2; // skip "0b"
                while let Some(b) = self.peek() {
                    if b == b'0' || b == b'1' || b == b'_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            } else {
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() || b == b'_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        // Check for decimal point followed by digit → floating-point literal.
        // Must not be hex/binary, and next char must be '.' followed by a digit
        // (to avoid confusing `1..10` range with `1.` float).
        let is_float = !is_hex
            && self.pos < self.bytes.len()
            && self.bytes[self.pos] == b'.'
            && self.pos + 1 < self.bytes.len()
            && self.bytes[self.pos + 1].is_ascii_digit();
        if is_float {
            self.pos += 1; // consume '.'
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // Optional exponent: e/E [+/-] digits
            if self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'e' || self.bytes[self.pos] == b'E')
            {
                self.pos += 1;
                if self.pos < self.bytes.len()
                    && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-')
                {
                    self.pos += 1;
                }
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() || b == b'_' {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
            // Optional f/F suffix → FloatLit instead of DoubleLit.
            let is_float_suffix = self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'f' || self.bytes[self.pos] == b'F');
            if is_float_suffix {
                self.pos += 1;
            }
            let raw = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII float");
            let s: String = raw
                .chars()
                .filter(|c| *c != '_' && *c != 'f' && *c != 'F')
                .collect();
            let token_kind = if is_float_suffix {
                TokenKind::FloatLit
            } else {
                TokenKind::DoubleLit
            };
            match s.parse::<f64>() {
                Ok(v) => self.emit(token_kind, start, self.pos, Some(TokenPayload::Double(v))),
                Err(_) => {
                    self.error(
                        start,
                        self.pos,
                        format!("floating-point literal {s} invalid"),
                    );
                }
            }
            return;
        }
        // Check for exponent without decimal point: 1e10, 2E5
        let has_exponent = !is_hex
            && self.pos < self.bytes.len()
            && (self.bytes[self.pos] == b'e' || self.bytes[self.pos] == b'E');
        if has_exponent {
            self.pos += 1;
            if self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-')
            {
                self.pos += 1;
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() || b == b'_' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'f' || self.bytes[self.pos] == b'F')
            {
                self.pos += 1;
            }
            let raw = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII float");
            let s: String = raw
                .chars()
                .filter(|c| *c != '_' && *c != 'f' && *c != 'F')
                .collect();
            match s.parse::<f64>() {
                Ok(v) => self.emit(
                    TokenKind::DoubleLit,
                    start,
                    self.pos,
                    Some(TokenPayload::Double(v)),
                ),
                Err(_) => {
                    self.error(
                        start,
                        self.pos,
                        format!("floating-point literal {s} invalid"),
                    );
                }
            }
            return;
        }
        // Consume optional `L` suffix for Long literals.
        let is_long = self.pos < self.bytes.len() && self.bytes[self.pos] == b'L';
        if is_long {
            self.pos += 1;
        }
        // Check for trailing f/F suffix → float literal without decimal point: 42f
        if self.pos < self.bytes.len()
            && (self.bytes[self.pos] == b'f' || self.bytes[self.pos] == b'F')
        {
            self.pos += 1;
            let raw = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII float");
            let s: String = raw
                .chars()
                .filter(|c| *c != '_' && *c != 'f' && *c != 'F')
                .collect();
            match s.parse::<f64>() {
                Ok(v) => self.emit(
                    TokenKind::DoubleLit,
                    start,
                    self.pos,
                    Some(TokenPayload::Double(v)),
                ),
                Err(_) => {
                    self.error(
                        start,
                        self.pos,
                        format!("floating-point literal {s} invalid"),
                    );
                }
            }
            return;
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos])
            .expect("ASCII digits")
            .trim_end_matches('L');
        // Remove underscores for parsing
        let s: String = raw.chars().filter(|c| *c != '_').collect();
        let parsed = if s.starts_with("0x") || s.starts_with("0X") {
            i64::from_str_radix(&s[2..], 16)
        } else if s.starts_with("0b") || s.starts_with("0B") {
            i64::from_str_radix(&s[2..], 2)
        } else {
            s.parse::<i64>()
        };
        match parsed {
            Ok(v) => {
                if is_long {
                    self.emit(
                        TokenKind::LongLit,
                        start,
                        self.pos,
                        Some(TokenPayload::Int(v)),
                    );
                } else {
                    self.emit(
                        TokenKind::IntLit,
                        start,
                        self.pos,
                        Some(TokenPayload::Int(v)),
                    );
                }
            }
            Err(_) => {
                self.error(start, self.pos, format!("integer literal {s} out of range"));
            }
        }
    }

    /// Scan a string literal in templated form. The opening `"` is at
    /// `self.pos`. Emits `StringStart`, then a sequence of chunks /
    /// interpolations, then `StringEnd`.
    ///
    /// If the opening quote is actually a `"""` (triple-quoted),
    /// dispatches to [`scan_raw_string`] which has its own scanning
    /// rules: no escape sequences, newlines are allowed, and the
    /// terminator is the matching `"""`.
    fn scan_string(&mut self) {
        // Triple-quoted raw string: `"""..."""`. Detected here so we
        // never enter the normal scanner's escape-aware path for
        // raw strings.
        if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') {
            self.scan_raw_string();
            return;
        }

        let open = self.pos;
        self.pos += 1; // consume opening "
        self.emit(TokenKind::StringStart, open, self.pos, None);

        let mut chunk_start = self.pos;
        let mut chunk = String::new();

        loop {
            let Some(b) = self.peek() else {
                self.error(open, self.pos, "unterminated string literal");
                self.emit(TokenKind::StringEnd, self.pos, self.pos, None);
                return;
            };
            match b {
                b'"' => {
                    if !chunk.is_empty() {
                        self.emit(
                            TokenKind::StringChunk,
                            chunk_start,
                            self.pos,
                            Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                        );
                    }
                    let close_start = self.pos;
                    self.pos += 1;
                    self.emit(TokenKind::StringEnd, close_start, self.pos, None);
                    return;
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        self.error(esc_start, self.pos, "trailing backslash in string");
                        return;
                    };
                    self.pos += 1;
                    match esc {
                        b'n' => chunk.push('\n'),
                        b'r' => chunk.push('\r'),
                        b't' => chunk.push('\t'),
                        b'\\' => chunk.push('\\'),
                        b'"' => chunk.push('"'),
                        b'$' => chunk.push('$'),
                        b'0' => chunk.push('\0'),
                        other => {
                            self.error(
                                esc_start,
                                self.pos,
                                format!("unknown escape sequence \\{}", other as char),
                            );
                        }
                    }
                }
                b'$' => {
                    if !chunk.is_empty() {
                        self.emit(
                            TokenKind::StringChunk,
                            chunk_start,
                            self.pos,
                            Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                        );
                    }
                    let dollar = self.pos;
                    self.pos += 1;
                    if self.peek() == Some(b'{') {
                        // ${ ... } interpolation: re-enter normal lex mode
                        // tracking brace depth.
                        self.pos += 1;
                        self.emit(TokenKind::StringExprStart, dollar, self.pos, None);
                        self.scan_interpolated_expr();
                    } else if matches!(self.peek(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
                    {
                        // $ident interpolation.
                        let id_start = self.pos;
                        while let Some(b) = self.peek() {
                            if b.is_ascii_alphanumeric() || b == b'_' {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                        let text = std::str::from_utf8(&self.bytes[id_start..self.pos])
                            .expect("ASCII ident")
                            .to_string();
                        self.emit(
                            TokenKind::StringIdentRef,
                            dollar,
                            self.pos,
                            Some(TokenPayload::StringIdentRef(text)),
                        );
                    } else {
                        // Lone `$` — treat as literal.
                        chunk.push('$');
                    }
                    chunk_start = self.pos;
                }
                b'\n' => {
                    self.error(open, self.pos, "newline in string literal");
                    return;
                }
                _ => {
                    // Append the next UTF-8 char to the chunk verbatim.
                    let ch_len = utf8_char_len(b);
                    let end = (self.pos + ch_len).min(self.bytes.len());
                    let s = std::str::from_utf8(&self.bytes[self.pos..end])
                        .expect("valid UTF-8 in source");
                    chunk.push_str(s);
                    self.pos = end;
                }
            }
        }
    }

    /// Scan a triple-quoted raw string `"""...***"""`.
    ///
    /// Raw strings are simpler than regular strings:
    ///
    /// - **No escape interpretation.** A backslash is a literal
    ///   backslash; `\n` is the two characters `\` and `n`, not a
    ///   newline.
    /// - **Newlines are allowed** in the body. Regular strings reject
    ///   newlines as a syntax error; raw strings preserve them
    ///   verbatim.
    /// - **No `$` interpolation in PR scope.** Kotlin's spec allows
    ///   `$ident` and `${expr}` inside raw strings, but no current
    ///   fixture exercises that. The `$` character is preserved as-is
    ///   here. A future PR can add interpolation by mirroring the
    ///   regular-string code path.
    /// - **The terminator is exactly `"""`.** The first `"""`
    ///   sequence ends the literal. (Kotlin's "longest match" rule
    ///   for trailing quotes is also out of scope; if a future
    ///   fixture needs `""""..."""` we'll revisit.)
    ///
    /// Emits the same `StringStart` / `StringChunk` / `StringEnd`
    /// shape as regular strings so the parser doesn't need a separate
    /// path. The whole content is a single `StringChunk` because raw
    /// strings have no interpolation in PR scope.
    fn scan_raw_string(&mut self) {
        let open = self.pos;
        self.pos += 3; // consume opening """
        self.emit(TokenKind::StringStart, open, self.pos, None);

        let chunk_start = self.pos;
        let mut chunk = String::new();

        loop {
            // Closing """ — first wins.
            if self.peek() == Some(b'"')
                && self.peek_at(1) == Some(b'"')
                && self.peek_at(2) == Some(b'"')
            {
                if !chunk.is_empty() {
                    self.emit(
                        TokenKind::StringChunk,
                        chunk_start,
                        self.pos,
                        Some(TokenPayload::StringChunk(chunk)),
                    );
                }
                let close_start = self.pos;
                self.pos += 3;
                self.emit(TokenKind::StringEnd, close_start, self.pos, None);
                return;
            }

            let Some(b) = self.peek() else {
                self.error(open, self.pos, "unterminated raw string literal");
                self.emit(TokenKind::StringEnd, self.pos, self.pos, None);
                return;
            };

            // Append the next UTF-8 character verbatim. Newlines,
            // backslashes, dollar signs, and lone quotes all pass
            // through unchanged.
            let ch_len = utf8_char_len(b);
            let end = (self.pos + ch_len).min(self.bytes.len());
            let s = std::str::from_utf8(&self.bytes[self.pos..end]).expect("valid UTF-8 in source");
            chunk.push_str(s);
            self.pos = end;
        }
    }

    /// Lex tokens inside a `${ … }` interpolation, tracking brace depth
    /// so nested `{` `}` don't terminate the interpolation early. Emits a
    /// `StringExprEnd` token on the matching `}`.
    fn scan_interpolated_expr(&mut self) {
        let mut depth: u32 = 1;
        loop {
            // Skip whitespace (not newlines — but interpolations on a
            // single line is what fixtures exercise; multi-line is fine).
            while let Some(b) = self.peek() {
                if b == b' ' || b == b'\t' || b == b'\r' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            let Some(b) = self.peek() else {
                self.error(self.pos, self.pos, "unterminated string interpolation");
                return;
            };
            match b {
                b'{' => {
                    let s = self.pos;
                    self.pos += 1;
                    self.emit(TokenKind::LBrace, s, self.pos, None);
                    depth += 1;
                }
                b'}' => {
                    let s = self.pos;
                    self.pos += 1;
                    depth -= 1;
                    if depth == 0 {
                        self.emit(TokenKind::StringExprEnd, s, self.pos, None);
                        return;
                    }
                    self.emit(TokenKind::RBrace, s, self.pos, None);
                }
                _ => self.next_token(),
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn emit(&mut self, kind: TokenKind, start: usize, end: usize, payload: Option<TokenPayload>) {
        self.tokens.push(Token::new(
            kind,
            Span::new(self.file, start as u32, end as u32),
        ));
        self.payloads.push(payload);
    }

    fn error(&mut self, start: usize, end: usize, msg: impl Into<String>) {
        let span = Span::new(self.file, start as u32, end as u32);
        self.diags.push(Diagnostic::error(span, msg));
        self.emit(TokenKind::Error, start, end, None);
    }
}

fn keyword_kind(text: &str) -> Option<TokenKind> {
    Some(match text {
        "fun" => TokenKind::KwFun,
        "val" => TokenKind::KwVal,
        "var" => TokenKind::KwVar,
        "if" => TokenKind::KwIf,
        "else" => TokenKind::KwElse,
        "return" => TokenKind::KwReturn,
        "while" => TokenKind::KwWhile,
        "do" => TokenKind::KwDo,
        "when" => TokenKind::KwWhen,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "break" => TokenKind::KwBreak,
        "continue" => TokenKind::KwContinue,
        "true" => TokenKind::KwTrue,
        "false" => TokenKind::KwFalse,
        "null" => TokenKind::KwNull,
        "class" => TokenKind::KwClass,
        "object" => TokenKind::KwObject,
        "package" => TokenKind::KwPackage,
        "import" => TokenKind::KwImport,
        "const" => TokenKind::KwConst,
        "throw" => TokenKind::KwThrow,
        "try" => TokenKind::KwTry,
        "catch" => TokenKind::KwCatch,
        "finally" => TokenKind::KwFinally,
        "is" => TokenKind::KwIs,
        "as" => TokenKind::KwAs,
        "init" => TokenKind::KwInit,
        "data" => TokenKind::KwData,
        "enum" => TokenKind::KwEnum,
        "interface" => TokenKind::KwInterface,
        "super" => TokenKind::KwSuper,
        "sealed" => TokenKind::KwSealed,
        "override" => TokenKind::KwOverride,
        "infix" => TokenKind::KwInfix,
        "inline" => TokenKind::KwInline,
        "open" => TokenKind::KwOpen,
        "abstract" => TokenKind::KwAbstract,
        "private" => TokenKind::KwPrivate,
        "protected" => TokenKind::KwProtected,
        "internal" => TokenKind::KwInternal,
        "operator" => TokenKind::KwOperator,
        "vararg" => TokenKind::KwVararg,
        "constructor" => TokenKind::KwConstructor,
        "lateinit" => TokenKind::KwLateinit,
        "suspend" => TokenKind::KwSuspend,
        "tailrec" => TokenKind::KwTailrec,
        _ => return None,
    })
}

/// Length in bytes of the UTF-8 character starting with byte `b`.
/// Continuation bytes (`0x80..0xC0`) shouldn't appear at a boundary;
/// we treat them as one-byte to make forward progress.
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0x80..=0xBF => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skotch_span::FileId;

    fn lex_str(src: &str) -> (LexedFile, Diagnostics) {
        let mut diags = Diagnostics::new();
        let lf = lex(FileId(0), src, &mut diags);
        (lf, diags)
    }

    fn kinds(lf: &LexedFile) -> Vec<TokenKind> {
        lf.tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lex_empty() {
        let (lf, d) = lex_str("");
        assert!(d.is_empty());
        assert_eq!(kinds(&lf), vec![TokenKind::Eof]);
    }

    #[test]
    fn lex_fun_main() {
        let (lf, d) = lex_str("fun main() {}");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::KwFun,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_string_literal_simple() {
        let (lf, d) = lex_str(r#""hello""#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::StringStart,
                TokenKind::StringChunk,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            lf.payload(1),
            Some(&TokenPayload::StringChunk("hello".to_string()))
        );
    }

    #[test]
    fn lex_int_literal() {
        let (lf, d) = lex_str("42");
        assert!(d.is_empty());
        assert_eq!(kinds(&lf), vec![TokenKind::IntLit, TokenKind::Eof]);
        assert_eq!(lf.payload(0), Some(&TokenPayload::Int(42)));
    }

    #[test]
    fn lex_string_template_with_ident() {
        let (lf, d) = lex_str(r#""Hello, $name!""#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::StringStart,
                TokenKind::StringChunk,
                TokenKind::StringIdentRef,
                TokenKind::StringChunk,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            lf.payload(1),
            Some(&TokenPayload::StringChunk("Hello, ".to_string()))
        );
        assert_eq!(
            lf.payload(2),
            Some(&TokenPayload::StringIdentRef("name".to_string()))
        );
    }

    #[test]
    fn lex_string_template_with_expr() {
        let (lf, d) = lex_str(r#""value: ${1 + 2}""#);
        assert!(d.is_empty(), "{:?}", d);
        // Expect: Start, Chunk, ExprStart, IntLit, Plus, IntLit, ExprEnd, End, Eof
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::StringStart,
                TokenKind::StringChunk,
                TokenKind::StringExprStart,
                TokenKind::IntLit,
                TokenKind::Plus,
                TokenKind::IntLit,
                TokenKind::StringExprEnd,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_println_call() {
        let (lf, d) = lex_str(r#"println("Hello, world!")"#);
        assert!(d.is_empty());
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::StringStart,
                TokenKind::StringChunk,
                TokenKind::StringEnd,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_newlines_collapse() {
        let (lf, d) = lex_str("a\n\n\nb");
        assert!(d.is_empty());
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_line_comment_skipped() {
        let (lf, d) = lex_str("a // comment\nb");
        assert!(d.is_empty());
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_raw_string_simple() {
        let (lf, d) = lex_str(r#""""hello""""#);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::StringStart,
                TokenKind::StringChunk,
                TokenKind::StringEnd,
                TokenKind::Eof,
            ]
        );
        assert_eq!(
            lf.payload(1),
            Some(&TokenPayload::StringChunk("hello".to_string()))
        );
    }

    #[test]
    fn lex_raw_string_preserves_newlines() {
        let src = "\"\"\"line one\nline two\"\"\"";
        let (lf, d) = lex_str(src);
        assert!(d.is_empty(), "{:?}", d);
        // Single chunk containing the literal newline.
        let chunks: Vec<_> = lf
            .payloads
            .iter()
            .filter_map(|p| match p {
                Some(TokenPayload::StringChunk(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, vec!["line one\nline two".to_string()]);
    }

    #[test]
    fn lex_raw_string_does_not_interpret_escapes() {
        // `\n` is the literal two characters, NOT a newline.
        let (lf, _d) = lex_str(r#""""a\nb""""#);
        let chunks: Vec<_> = lf
            .payloads
            .iter()
            .filter_map(|p| match p {
                Some(TokenPayload::StringChunk(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, vec![r#"a\nb"#.to_string()]);
    }

    #[test]
    fn lex_raw_string_inside_a_function() {
        let src = r#"
            fun main() {
                val s = """hello"""
                println(s)
            }
        "#;
        let (lf, d) = lex_str(src);
        assert!(d.is_empty(), "{:?}", d);
        // The raw string produces the same StringStart/Chunk/End shape
        // as a regular string, so the parser handles it transparently.
        let chunks: Vec<_> = lf
            .payloads
            .iter()
            .filter_map(|p| match p {
                Some(TokenPayload::StringChunk(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks, vec!["hello".to_string()]);
    }

    #[test]
    fn lex_suspend_keyword() {
        // `suspend fun` should produce KwSuspend, KwFun, ...
        // The `suspend` modifier is recognised but the CPS transform
        // that would make a suspend function actually suspend is still
        // outstanding (v0.9.0 milestone).
        let (lf, d) = lex_str("suspend fun compute() {}");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::KwSuspend,
                TokenKind::KwFun,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }

    // ─── future test stubs ───────────────────────────────────────────────
    // TODO: lex_raw_string_with_interpolation — Kotlin allows $ident
    //                                            and ${expr} inside """..."""
    // TODO: lex_char_literal               — 'a', '\n'
    // TODO: lex_int_literals_all_bases     — 0xff, 0b1010, 100_000, 1L
    // TODO: lex_float_literals             — 3.14, 2.5e10, 1.0f
    // TODO: lex_keywords_disambiguation    — `class` vs `classification`
    // TODO: lex_operators_compound         — +=, -=, ::, ?:, ?.
    // TODO: lex_block_comment_nested       — Kotlin allows nested /* /* */ */
}
