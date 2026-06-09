//! Hand-rolled lexer for the subset of Kotlin 2 that skotch accepts.
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
    /// Parsed integer value. Currently only supports decimal `i64`.
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

/// Knobs that tune what the lexer emits. The FIR-compilation path uses
/// [`LexerOptions::default()`] (whitespace + comments dropped, matching
/// historical behavior). The SIL/CST path passes
/// `LexerOptions { preserve_trivia: true }` so that every byte of the
/// source becomes a token and can be reconstructed downstream.
#[derive(Copy, Clone, Debug, Default)]
pub struct LexerOptions {
    /// When true, emit `Whitespace`, `LineComment`, `BlockComment`, and
    /// `DocComment` tokens instead of silently consuming them.
    pub preserve_trivia: bool,
}

/// Lex `source` belonging to `file`. Errors are pushed into `diags` and
/// the lexer attempts to continue, marking failed runs with `Error`
/// tokens. The parser stops at the first `Error` it sees.
///
/// Equivalent to [`lex_with`] called with `LexerOptions::default()` —
/// kept as the canonical entry point for the FIR compilation pipeline.
pub fn lex(file: FileId, source: &str, diags: &mut Diagnostics) -> LexedFile {
    lex_with(file, source, diags, LexerOptions::default())
}

/// Lex `source` with explicit options. Used by the SIL/CST pipeline to
/// request trivia preservation; FIR callers should keep using [`lex`].
pub fn lex_with(
    file: FileId,
    source: &str,
    diags: &mut Diagnostics,
    options: LexerOptions,
) -> LexedFile {
    let mut lx = Lexer {
        file,
        bytes: source.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        payloads: Vec::new(),
        diags,
        options,
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
    options: LexerOptions,
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
        if self.options.preserve_trivia {
            // SIL mode: whitespace and newlines merge into one
            // `Whitespace` token whose text spans the entire run.
            // kotlinc's PSI uses the same convention — it has no
            // separate `NEWLINE` element — and matching that shape
            // is required for byte-identical SIL YAML.
            let ws_start = self.pos;
            while let Some(b) = self.peek() {
                if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos > ws_start {
                self.emit(TokenKind::Whitespace, ws_start, self.pos, None);
                return;
            }
        } else {
            // FIR mode: inline whitespace consumed silently; newlines
            // emitted as a separate `Newline` token so the FIR
            // parser's newline-sensitive grammar can react. This is
            // the historical behavior — every caller of `lex()` sees
            // it.
            while let Some(b) = self.peek() {
                if b == b' ' || b == b'\t' || b == b'\r' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let start = self.pos;
        let Some(b) = self.peek() else { return };

        // FIR mode only: collapse a run of `\n`s into one `Newline`
        // token. SIL mode handled the newline above as part of the
        // unified whitespace token.
        if b == b'\n' && !self.options.preserve_trivia {
            while let Some(b'\n') = self.peek() {
                self.pos += 1;
            }
            self.emit(TokenKind::Newline, start, self.pos, None);
            return;
        }

        // Line comment: `// ... \n`. The trailing `\n` is **not** part of
        // the comment — it stays a separate `Newline` token so the
        // parser's newline-sensitivity logic still fires.
        if b == b'/' && self.peek_at(1) == Some(b'/') {
            while let Some(b) = self.peek() {
                if b == b'\n' {
                    break;
                }
                self.pos += 1;
            }
            if self.options.preserve_trivia {
                self.emit(TokenKind::LineComment, start, self.pos, None);
            }
            return;
        }

        // Block comment: `/* ... */`. Not nested-aware yet. A `/**`
        // (KDoc) is emitted as `DocComment`, plain `/*` as
        // `BlockComment`. The FIR-path lexer (no preserve_trivia) drops
        // both equally.
        if b == b'/' && self.peek_at(1) == Some(b'*') {
            let is_doc = self.peek_at(2) == Some(b'*') && self.peek_at(3) != Some(b'/');
            self.pos += 2;
            while self.pos < self.bytes.len() {
                if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                    self.pos += 2;
                    if self.options.preserve_trivia {
                        let kind = if is_doc {
                            TokenKind::DocComment
                        } else {
                            TokenKind::BlockComment
                        };
                        self.emit(kind, start, self.pos, None);
                    }
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

        // Integer literals: `[0-9]+`. Hex / float handled elsewhere.
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
            b'.' if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos + 1].is_ascii_digit() =>
            {
                // `.3f` — float literal without leading zero.
                self.pos += 1; // advance past '.'
                self.lex_dot_number();
                return;
            }
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
        // Trailing f/F suffix → Float literal without decimal point: 42f
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
                    TokenKind::FloatLit,
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
                    // Flush any pending literal-content chunk so the
                    // escape sequence emerges as its own StringChunk
                    // token. kotlinc PSI wraps each escape in a
                    // dedicated ESCAPE_STRING_TEMPLATE_ENTRY, so
                    // splitting at the lexer level keeps the parser
                    // arms 1:1 with PSI nodes.
                    if !chunk.is_empty() {
                        self.emit(
                            TokenKind::StringChunk,
                            chunk_start,
                            self.pos,
                            Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                        );
                    }
                    let esc_start = self.pos;
                    self.pos += 1;
                    let Some(esc) = self.peek() else {
                        self.error(esc_start, self.pos, "trailing backslash in string");
                        return;
                    };
                    self.pos += 1;
                    let mut esc_chunk = String::new();
                    match esc {
                        b'n' => esc_chunk.push('\n'),
                        b'r' => esc_chunk.push('\r'),
                        b't' => esc_chunk.push('\t'),
                        b'\\' => esc_chunk.push('\\'),
                        b'"' => esc_chunk.push('"'),
                        b'\'' => esc_chunk.push('\''),
                        b'$' => esc_chunk.push('$'),
                        b'0' => esc_chunk.push('\0'),
                        b'u' => {
                            // Unicode escape: \uXXXX (4 hex digits)
                            let mut hex = String::with_capacity(4);
                            for _ in 0..4 {
                                if let Some(&h) = self.bytes.get(self.pos) {
                                    if h.is_ascii_hexdigit() {
                                        hex.push(h as char);
                                        self.pos += 1;
                                    } else {
                                        break;
                                    }
                                }
                            }
                            if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                                if let Some(ch) = char::from_u32(cp) {
                                    esc_chunk.push(ch);
                                }
                            }
                        }
                        other => {
                            self.error(
                                esc_start,
                                self.pos,
                                format!("unknown escape sequence \\{}", other as char),
                            );
                        }
                    }
                    self.emit(
                        TokenKind::StringChunk,
                        esc_start,
                        self.pos,
                        Some(TokenPayload::StringChunk(esc_chunk)),
                    );
                    chunk_start = self.pos;
                }
                b'$' => {
                    let dollar = self.pos;
                    let lookahead = self.bytes.get(dollar + 1).copied();
                    let is_template_open = lookahead == Some(b'{')
                        || matches!(lookahead, Some(b) if b.is_ascii_alphabetic() || b == b'_');
                    if is_template_open {
                        if !chunk.is_empty() {
                            self.emit(
                                TokenKind::StringChunk,
                                chunk_start,
                                dollar,
                                Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                            );
                        }
                        self.pos += 1; // past `$`
                        if self.peek() == Some(b'{') {
                            self.pos += 1;
                            self.emit(TokenKind::StringExprStart, dollar, self.pos, None);
                            self.scan_interpolated_expr();
                        } else {
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
                        }
                        chunk_start = self.pos;
                    } else {
                        // Lone `$` — literal in the running chunk.
                        chunk.push('$');
                        self.pos += 1;
                    }
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
    /// Raw strings differ from regular strings in:
    ///
    /// - **No escape interpretation.** A backslash is a literal
    ///   backslash; `\n` is the two characters `\` and `n`, not a
    ///   newline.
    /// - **Newlines are allowed** in the body. Regular strings reject
    ///   newlines as a syntax error; raw strings preserve them
    ///   verbatim.
    /// - **`$ident` and `${expr}` interpolation IS supported** (matches
    ///   Kotlin spec). Added to support the common
    ///   `"""...$name...""".trimIndent()` idiom — surfaced by
    ///   parity/47-raw-strings. Mirrors the regular-string $-handling
    ///   at scan_string:~644.
    /// - **The terminator is exactly `"""`.** The first `"""`
    ///   sequence ends the literal. (Kotlin's "longest match" rule
    ///   for trailing quotes is also out of scope.)
    ///
    /// Emits the same `StringStart` / `StringChunk` / `StringIdentRef`
    /// / `StringExprStart` / `StringEnd` shape as regular strings.
    fn scan_raw_string(&mut self) {
        let open = self.pos;
        self.pos += 3; // consume opening """
        self.emit(TokenKind::StringStart, open, self.pos, None);

        let mut chunk_start = self.pos;
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

            if b == b'$' {
                let dollar = self.pos;
                let lookahead = self.bytes.get(dollar + 1).copied();
                let is_template_open = lookahead == Some(b'{')
                    || matches!(lookahead, Some(b) if b.is_ascii_alphabetic() || b == b'_');
                if is_template_open {
                    // Flush the running chunk only when we're really
                    // about to start a template entry — otherwise the
                    // `$` is just a literal that joins the chunk.
                    if !chunk.is_empty() {
                        self.emit(
                            TokenKind::StringChunk,
                            chunk_start,
                            dollar,
                            Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                        );
                    }
                    self.pos += 1; // past `$`
                    if self.peek() == Some(b'{') {
                        self.pos += 1;
                        self.emit(TokenKind::StringExprStart, dollar, self.pos, None);
                        self.scan_interpolated_expr();
                    } else {
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
                    }
                    chunk_start = self.pos;
                } else {
                    // Lone `$` (followed by `)`, `@`, digit, EOF, …)
                    // — kotlinc PSI still splits the chunk at the `$`:
                    // the preceding literal becomes its own
                    // LITERAL_STRING_TEMPLATE_ENTRY, then the `$` is
                    // its own one-character literal entry, then the
                    // remainder picks up as a fresh chunk.
                    if !chunk.is_empty() {
                        self.emit(
                            TokenKind::StringChunk,
                            chunk_start,
                            dollar,
                            Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                        );
                    }
                    self.pos += 1;
                    self.emit(
                        TokenKind::StringChunk,
                        dollar,
                        self.pos,
                        Some(TokenPayload::StringChunk("$".to_string())),
                    );
                    chunk_start = self.pos;
                }
                continue;
            }

            // kotlinc PSI splits a raw string at every backslash —
            // each `\` becomes its own LITERAL_STRING_TEMPLATE_ENTRY,
            // even though no escape interpretation happens. Flush the
            // running chunk, emit the lone `\` as its own chunk, and
            // continue.
            if b == b'\\' {
                if !chunk.is_empty() {
                    self.emit(
                        TokenKind::StringChunk,
                        chunk_start,
                        self.pos,
                        Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                    );
                }
                let bs_start = self.pos;
                self.pos += 1;
                self.emit(
                    TokenKind::StringChunk,
                    bs_start,
                    self.pos,
                    Some(TokenPayload::StringChunk("\\".to_string())),
                );
                chunk_start = self.pos;
                continue;
            }

            // kotlinc PSI splits a raw string at every embedded `"`
            // (each `"` becomes its own LITERAL_STRING_TEMPLATE_ENTRY)
            // — but not at the closing `"""`, which terminates the
            // literal entirely. A single `"` inside a triple-quoted
            // string is fine: it would only be the closing delimiter
            // if it were followed by two more quotes, which the loop
            // header already checked for.
            if b == b'"' {
                if !chunk.is_empty() {
                    self.emit(
                        TokenKind::StringChunk,
                        chunk_start,
                        self.pos,
                        Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                    );
                }
                let q_start = self.pos;
                self.pos += 1;
                self.emit(
                    TokenKind::StringChunk,
                    q_start,
                    self.pos,
                    Some(TokenPayload::StringChunk("\"".to_string())),
                );
                chunk_start = self.pos;
                continue;
            }

            // kotlinc PSI also splits each `\n` (or `\r\n`) inside a
            // raw string as its own LITERAL_STRING_TEMPLATE_ENTRY.
            if b == b'\n' || b == b'\r' {
                if !chunk.is_empty() {
                    self.emit(
                        TokenKind::StringChunk,
                        chunk_start,
                        self.pos,
                        Some(TokenPayload::StringChunk(std::mem::take(&mut chunk))),
                    );
                }
                let nl_start = self.pos;
                // Consume `\r\n` together to match the source text.
                if b == b'\r' && self.peek_at(1) == Some(b'\n') {
                    self.pos += 2;
                } else {
                    self.pos += 1;
                }
                let nl_text = std::str::from_utf8(&self.bytes[nl_start..self.pos])
                    .expect("valid UTF-8 in source")
                    .to_string();
                self.emit(
                    TokenKind::StringChunk,
                    nl_start,
                    self.pos,
                    Some(TokenPayload::StringChunk(nl_text)),
                );
                chunk_start = self.pos;
                continue;
            }

            // Append the next UTF-8 character verbatim. Lone quotes
            // pass through unchanged.
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

    /// Lex a float literal starting with `.`: `.3f`, `.5`, `.123e4`
    fn lex_dot_number(&mut self) -> Token {
        let start = self.pos - 1; // include the leading '.'
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        // Exponent
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
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        let is_float = self.pos < self.bytes.len()
            && (self.bytes[self.pos] == b'f' || self.bytes[self.pos] == b'F');
        if is_float {
            self.pos += 1;
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII");
        let s: String = raw
            .chars()
            .filter(|c| *c != '_' && *c != 'f' && *c != 'F')
            .collect();
        let kind = if is_float {
            TokenKind::FloatLit
        } else {
            TokenKind::DoubleLit
        };
        let val = s.parse::<f64>().unwrap_or(0.0);
        self.emit(kind, start, self.pos, Some(TokenPayload::Double(val)));
        Token::new(kind, Span::new(self.file, start as u32, self.pos as u32))
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
        // kotlinc PSI emits each embedded `\n` as its own
        // LITERAL_STRING_TEMPLATE_ENTRY (each backslash and each
        // newline split the chunk), so the lexer surfaces them as
        // separate `StringChunk`s. The text is still preserved
        // verbatim — concatenating the chunks reproduces the source.
        let chunks: Vec<_> = lf
            .payloads
            .iter()
            .filter_map(|p| match p {
                Some(TokenPayload::StringChunk(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            chunks,
            vec![
                "line one".to_string(),
                "\n".to_string(),
                "line two".to_string()
            ]
        );
        assert_eq!(chunks.concat(), "line one\nline two");
    }

    #[test]
    fn lex_raw_string_does_not_interpret_escapes() {
        // `\n` inside a raw string is the two literal characters `\`
        // and `n`, NOT a newline. kotlinc PSI splits at the backslash,
        // so the lexer emits separate chunks (`a`, `\`, `nb`) — the
        // semantic invariant is that NO escape interpretation happens,
        // which we verify by reconstructing the source verbatim.
        let (lf, _d) = lex_str(r#""""a\nb""""#);
        let chunks: Vec<_> = lf
            .payloads
            .iter()
            .filter_map(|p| match p {
                Some(TokenPayload::StringChunk(s)) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            chunks,
            vec!["a".to_string(), "\\".to_string(), "nb".to_string()]
        );
        assert_eq!(chunks.concat(), r#"a\nb"#);
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

    // ─── trivia-mode tests (SIL/CST pipeline) ────────────────────────────

    fn lex_str_trivia(src: &str) -> (LexedFile, Diagnostics) {
        let mut diags = Diagnostics::new();
        let lf = lex_with(
            FileId(0),
            src,
            &mut diags,
            LexerOptions {
                preserve_trivia: true,
            },
        );
        (lf, diags)
    }

    fn token_spans(lf: &LexedFile) -> Vec<(TokenKind, std::ops::Range<u32>)> {
        lf.tokens
            .iter()
            .map(|t| (t.kind, t.span.start..t.span.end))
            .collect()
    }

    #[test]
    fn trivia_default_drops_whitespace_and_comments() {
        // The FIR-path lexer must keep its old behavior — no `Whitespace`
        // or `*Comment` tokens — so that all 14 downstream crates see
        // exactly the same token stream as before this refactor.
        let (lf, d) = lex_str("// hi\nfun  foo() {} /* bye */");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::Newline, // separates // hi from fun foo
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
    fn trivia_preserve_emits_whitespace_tokens() {
        let (lf, d) = lex_str_trivia("fun  foo() ");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::KwFun,
                TokenKind::Whitespace, // two spaces between fun and foo
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Whitespace, // trailing space
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn trivia_preserve_emits_line_comment() {
        let (lf, d) = lex_str_trivia("// hello\nfun x()");
        assert!(d.is_empty(), "{:?}", d);
        // In SIL mode (preserve_trivia=true), the trailing `\n` is
        // merged into the *following* `Whitespace` token — kotlinc's
        // PSI has no separate `NEWLINE` element, and we mirror that.
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::LineComment, // "// hello"
                TokenKind::Whitespace,  // "\n"
                TokenKind::KwFun,
                TokenKind::Whitespace,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
        let comment_span = lf.tokens[0].span;
        assert_eq!(comment_span.end - comment_span.start, 8); // "// hello".len()
        let ws_span = lf.tokens[1].span;
        assert_eq!(ws_span.end - ws_span.start, 1); // just "\n"
    }

    #[test]
    fn trivia_preserve_distinguishes_block_and_doc_comment() {
        let (lf, d) = lex_str_trivia("/* plain */ /** doc */");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            kinds(&lf),
            vec![
                TokenKind::BlockComment,
                TokenKind::Whitespace,
                TokenKind::DocComment,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn trivia_preserve_empty_doc_comment_is_block_not_doc() {
        // `/**/` is a block comment, not a doc comment — there's no
        // body between the `/**` and the `*/`.
        let (lf, d) = lex_str_trivia("/**/");
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(kinds(&lf), vec![TokenKind::BlockComment, TokenKind::Eof]);
    }

    #[test]
    fn trivia_preserve_roundtrips_source_bytes() {
        // Sum of all token spans must cover the full source range with
        // no gaps and no overlaps — this is the invariant the SIL
        // builder relies on for byte-for-byte reconstruction.
        let src = "// hi\n  fun foo() { /* x */ }\n";
        let (lf, _) = lex_str_trivia(src);
        let spans = token_spans(&lf);
        let mut cursor = 0u32;
        for (kind, range) in &spans {
            if *kind == TokenKind::Eof {
                assert_eq!(range.start as usize, src.len());
                break;
            }
            assert_eq!(range.start, cursor, "gap before {:?} at {}", kind, cursor);
            cursor = range.end;
        }
        assert_eq!(cursor as usize, src.len());
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
