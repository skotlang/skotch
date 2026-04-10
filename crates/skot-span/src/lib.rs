//! Source spans, file ids, and source maps for skot.
//!
//! This is a deliberately tiny crate. Every higher-layer crate depends on it,
//! so it must compile fast and have no transitive deps beyond `rustc-hash`.

use rustc_hash::FxHashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An opaque, cheap-to-copy handle to a source file in the [`SourceMap`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// A half-open byte range `[start, end)` within a single source file.
///
/// Spans are used by every layer above the lexer for diagnostics. They
/// reference a byte offset in the file's UTF-8 source text, **not** a
/// character offset.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span end ({end}) precedes start ({start})");
        Span { file, start, end }
    }

    /// A zero-width span at the given byte offset. Used as a placeholder
    /// for synthetic AST nodes that have no source location.
    pub fn empty(file: FileId) -> Self {
        Span {
            file,
            start: 0,
            end: 0,
        }
    }

    /// Combine two spans into the smallest enclosing span. Both spans must
    /// be in the same file or this will panic in debug builds.
    pub fn merge(self, other: Span) -> Span {
        debug_assert_eq!(self.file, other.file, "cannot merge spans across files");
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// One source file's metadata: path on disk and the full UTF-8 text.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub text: Arc<String>,
}

impl SourceFile {
    /// Slice the file's text for the given span.
    pub fn slice(&self, span: Span) -> &str {
        debug_assert_eq!(span.file, self.id);
        &self.text[span.start as usize..span.end as usize]
    }

    /// Convert a byte offset into a 1-based `(line, column)` pair. The
    /// column counts UTF-8 *characters*, not bytes.
    pub fn line_col(&self, byte_offset: u32) -> (u32, u32) {
        let mut line = 1u32;
        let mut col = 1u32;
        for (i, ch) in self.text.char_indices() {
            if i as u32 >= byte_offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

/// The source map owns every loaded `SourceFile` and hands out [`FileId`]s.
///
/// Skot keeps the source map immutable after the front-end has finished
/// loading inputs; it is therefore cheap to share between threads.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    by_path: FxHashMap<PathBuf, FileId>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a file to the source map and return its [`FileId`]. Files added
    /// twice via the same path return the existing id.
    pub fn add(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) -> FileId {
        let path = path.into();
        if let Some(&id) = self.by_path.get(&path) {
            return id;
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile {
            id,
            path: path.clone(),
            text: Arc::new(text.into()),
        });
        self.by_path.insert(path, id);
        id
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = &SourceFile> {
        self.files.iter()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn lookup_by_path(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(path).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_lookup() {
        let mut sm = SourceMap::new();
        let id = sm.add("/tmp/Hello.kt", "fun main() {}");
        assert_eq!(sm.get(id).path, std::path::PathBuf::from("/tmp/Hello.kt"));
        assert_eq!(sm.get(id).text.as_str(), "fun main() {}");
        // Re-adding the same path returns the same id.
        let id2 = sm.add("/tmp/Hello.kt", "irrelevant");
        assert_eq!(id, id2);
    }

    #[test]
    fn span_merge() {
        let f = FileId(0);
        let a = Span::new(f, 0, 3);
        let b = Span::new(f, 5, 8);
        assert_eq!(a.merge(b), Span::new(f, 0, 8));
    }

    #[test]
    fn line_col_basic() {
        let mut sm = SourceMap::new();
        let id = sm.add("/x", "abc\ndef\nghi");
        let f = sm.get(id);
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col(3), (1, 4));
        assert_eq!(f.line_col(4), (2, 1));
        assert_eq!(f.line_col(8), (3, 1));
    }
}
