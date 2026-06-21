//! Small utilities shared across the crate.
//!
//! Hosts the port of `ResourceUtils::StringBuilder` — the Android
//! resource string processor that interprets escape sequences, quoting,
//! and whitespace collapsing rules for string resources and XML
//! attribute values.

use crate::res::value::{StyleStringSpan, UntranslatableSection};

/// UTF-16 length of a string (surrogate pairs count as 2).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// A processed resource string: interpreted text plus style spans and
/// untranslatable sections. Mirrors `FlattenedXmlString`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessedString {
    pub text: String,
    pub spans: Vec<StyleStringSpan>,
    pub untranslatable_sections: Vec<UntranslatableSection>,
}

/// Interprets Android resource string syntax: escape sequences
/// (`\n`, `\t`, `\\`, `A`, `\@`, `\?`, `\#`, `\"`, `\'`),
/// double-quote toggled literal sections, and whitespace collapsing.
///
/// Port of `ResourceUtils::StringBuilder`. With `preserve_spaces`,
/// whitespace and quotes are kept verbatim and only escape sequences are
/// interpreted (used for XML attribute values).
pub struct StringBuilder {
    preserve_spaces: bool,
    quote: bool,
    last_codepoint_was_space: bool,
    error: Option<String>,
    out: ProcessedString,
    utf16_len: u32,
}

impl StringBuilder {
    pub fn new(preserve_spaces: bool) -> Self {
        StringBuilder {
            preserve_spaces,
            quote: preserve_spaces,
            last_codepoint_was_space: false,
            error: None,
            out: ProcessedString::default(),
            utf16_len: 0,
        }
    }

    pub fn append_text(&mut self, text: &str) -> &mut Self {
        if self.error.is_some() {
            return self;
        }
        let previous_len = self.out.text.len();
        let mut chars = text.chars().peekable();
        while let Some(codepoint) = chars.next() {
            if !self.preserve_spaces
                && !self.quote
                && codepoint.is_ascii()
                && (codepoint as u8 as char).is_ascii_whitespace()
            {
                if !self.last_codepoint_was_space {
                    self.out.text.push(' ');
                    self.last_codepoint_was_space = true;
                }
                continue;
            }
            self.last_codepoint_was_space = false;

            if codepoint == '\\' {
                if let Some(escaped) = chars.next() {
                    match escaped {
                        't' => self.out.text.push('\t'),
                        'n' => self.out.text.push('\n'),
                        '#' | '@' | '?' | '"' | '\'' | '\\' => self.out.text.push(escaped),
                        'u' => {
                            let mut code: u32 = 0;
                            let mut valid = true;
                            for _ in 0..4 {
                                match chars.next().and_then(|c| c.to_digit(16)) {
                                    Some(digit) => code = (code << 4) | digit,
                                    None => {
                                        valid = false;
                                        break;
                                    }
                                }
                            }
                            match char::from_u32(code).filter(|_| valid) {
                                Some(c) => self.out.text.push(c),
                                None => {
                                    self.error = Some(format!(
                                        "invalid unicode escape sequence in string\n\"{text}\""
                                    ));
                                    return self;
                                }
                            }
                        }
                        // Ignore the escape character; include the codepoint.
                        other => self.out.text.push(other),
                    }
                }
            } else if !self.preserve_spaces && codepoint == '"' {
                self.quote = !self.quote;
            } else if !self.preserve_spaces && !self.quote && codepoint == '\'' {
                self.error = Some(format!("unescaped apostrophe in string\n\"{text}\""));
                return self;
            } else {
                self.out.text.push(codepoint);
            }
        }
        self.utf16_len += utf16_len(&self.out.text[previous_len..]) as u32;
        self
    }

    /// Starts a style span; returns its handle. Span boundaries are
    /// expressed in UTF-16 offsets.
    pub fn start_span(&mut self, name: &str) -> usize {
        self.reset_text_state();
        self.out.spans.push(StyleStringSpan {
            name: name.to_string(),
            first_char: self.utf16_len,
            last_char: self.utf16_len,
        });
        self.out.spans.len() - 1
    }

    pub fn end_span(&mut self, handle: usize) {
        self.reset_text_state();
        if let Some(span) = self.out.spans.get_mut(handle) {
            span.last_char = self.utf16_len.saturating_sub(1);
        }
    }

    /// Starts an untranslatable section (byte offsets); returns its handle.
    pub fn start_untranslatable(&mut self) -> usize {
        let offset = self.out.text.len();
        self.out
            .untranslatable_sections
            .push(UntranslatableSection {
                start: offset,
                end: offset,
            });
        self.out.untranslatable_sections.len() - 1
    }

    pub fn end_untranslatable(&mut self, handle: usize) {
        let offset = self.out.text.len();
        if let Some(section) = self.out.untranslatable_sections.get_mut(handle) {
            section.end = offset;
        }
    }

    fn reset_text_state(&mut self) {
        // Starting/ending a span ends whitespace truncation and quotation.
        self.last_codepoint_was_space = false;
        self.quote = self.preserve_spaces;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn finish(self) -> Result<ProcessedString, String> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.out),
        }
    }

    /// The processed text so far (ignores any error).
    pub fn text(&self) -> &str {
        &self.out.text
    }
}

/// One-shot escape processing with spaces preserved (XML attribute
/// values). Mirrors `StringBuilder(true).AppendText(s).to_string()`.
pub fn process_string_preserve_spaces(s: &str) -> String {
    let mut builder = StringBuilder::new(true);
    builder.append_text(s);
    builder.out.text
}

/// Trims ASCII whitespace, mirroring `util::TrimWhitespace`.
pub fn trim_whitespace(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

/// Whether `s` is a valid Java identifier (used for package/class name
/// validation). Mirrors `util::IsJavaIdentifier` closely enough for
/// ASCII plus `$` and `_`.
pub fn is_java_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Whether `s` is a valid Android package name (dot-separated Java
/// identifiers, at least one dot not required here — mirrors
/// `util::IsAndroidPackageName` which requires a dot or "android").
pub fn is_android_package_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut saw_dot = false;
    for part in s.split('.') {
        if part.is_empty() {
            return false;
        }
        saw_dot |= true;
        if !is_java_identifier(part) {
            return false;
        }
    }
    let _ = saw_dot;
    s.contains('.') || s == "android"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_collapsing() {
        let mut b = StringBuilder::new(false);
        b.append_text("  hello\n\n world  ");
        assert_eq!(b.text(), " hello world ");
    }

    #[test]
    fn quotes_preserve_whitespace() {
        let mut b = StringBuilder::new(false);
        b.append_text("\"  hello  \"");
        assert_eq!(b.text(), "  hello  ");
    }

    #[test]
    fn escapes() {
        let mut b = StringBuilder::new(false);
        b.append_text(r"a\nb\tc\\d\@eA");
        assert_eq!(b.text(), "a\nb\tc\\d@eA");
    }

    #[test]
    fn unescaped_apostrophe_errors() {
        let mut b = StringBuilder::new(false);
        b.append_text("don't");
        assert!(b.error().is_some());
    }

    #[test]
    fn preserve_spaces_mode() {
        assert_eq!(process_string_preserve_spaces("  a  'b'  "), "  a  'b'  ");
        assert_eq!(process_string_preserve_spaces(r"é"), "é");
    }

    #[test]
    fn spans_track_utf16_offsets() {
        let mut b = StringBuilder::new(false);
        b.append_text("ab");
        let span = b.start_span("b");
        b.append_text("cd");
        b.end_span(span);
        let processed = b.finish().unwrap();
        assert_eq!(processed.spans[0].first_char, 2);
        assert_eq!(processed.spans[0].last_char, 3);
    }
}
