//! Text printer with lazy indentation.
//!
//! Port of `text/Printer.{h,cpp}`: indentation (two spaces per level)
//! is applied lazily before the first non-empty chunk of each line, so
//! a bare newline never emits trailing indent whitespace.

/// Mirrors `aapt::text::Printer`, writing into any `std::fmt::Write`
/// sink (a `String` in tests, the stdout buffer in the CLI).
pub struct Printer<'a> {
    out: &'a mut dyn std::fmt::Write,
    indent_level: usize,
    needs_indent: bool,
}

impl<'a> Printer<'a> {
    pub fn new(out: &'a mut dyn std::fmt::Write) -> Printer<'a> {
        Printer { out, indent_level: 0, needs_indent: false }
    }

    pub fn print(&mut self, s: impl AsRef<str>) -> &mut Self {
        let mut remaining = s.as_ref();
        while !remaining.is_empty() {
            let (chunk, rest, had_newline) = match remaining.find('\n') {
                Some(idx) => (&remaining[..idx], &remaining[idx + 1..], true),
                None => (remaining, "", false),
            };
            if !chunk.is_empty() {
                if self.needs_indent {
                    for _ in 0..self.indent_level {
                        let _ = self.out.write_str("  ");
                    }
                    self.needs_indent = false;
                }
                let _ = self.out.write_str(chunk);
            }
            if had_newline {
                let _ = self.out.write_str("\n");
                self.needs_indent = true;
            }
            remaining = rest;
        }
        self
    }

    pub fn println(&mut self, s: impl AsRef<str>) -> &mut Self {
        self.print(s);
        self.print("\n")
    }

    pub fn println_empty(&mut self) -> &mut Self {
        self.print("\n")
    }

    pub fn indent(&mut self) {
        self.indent_level += 1;
    }

    pub fn undent(&mut self) {
        self.indent_level = self.indent_level.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_is_lazy() {
        let mut buf = String::new();
        let mut p = Printer::new(&mut buf);
        p.println("a");
        p.indent();
        p.println("b\nc");
        p.println_empty();
        p.undent();
        p.print("d");
        assert_eq!(buf, "a\n  b\n  c\n\nd");
    }
}
