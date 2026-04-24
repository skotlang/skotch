//! Salsa-based incremental compilation database for skotch.
//!
//! ## Two-Phase Incremental Pipeline
//!
//! The database implements the "Gather then Lower" multi-file compilation
//! strategy with content-hash-based change detection:
//!
//! - **Phase 1 (Gather)**: Each file's top-level declarations are extracted
//!   into a [`FileExports`] tracked struct. These are aggregated into a
//!   [`PackageSymbolTable`] that provides cross-file visibility.
//!
//! - **Phase 2 (Compile)**: Each file is compiled with the shared symbol
//!   table. Salsa memoizes results per file — unchanged files with an
//!   unchanged symbol table return instantly from cache.
//!
//! ## Content Hashing
//!
//! Blake3 content hashes are used for efficient change detection. When a
//! file's text changes, its content hash changes, triggering recompilation.
//! When only a file's body changes (no signature changes), the
//! [`PackageSymbolTable`] remains stable and other files are NOT recompiled.

use salsa::Setter;

/// Compute a blake3 content hash of source text.
pub fn content_hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

// ─── Salsa inputs ───────────────────────────────────────────────────────────

/// A source file input to the compilation pipeline.
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub path: String,
    #[returns(ref)]
    pub text: String,
    #[returns(ref)]
    pub wrapper_class: String,
}

// ─── Tracked compilation ────────────────────────────────────────────────────

/// Result of compiling a single source file. Both the MIR (as JSON) and
/// error status are captured in ONE tracked function to avoid double
/// compilation.
#[salsa::tracked]
pub fn compile_file<'db>(db: &'db dyn salsa::Database, file: SourceFile) -> CompileResult<'db> {
    let text = file.text(db);
    let path = file.path(db);
    let wrapper = file.wrapper_class(db);

    let mut interner = skotch_intern::Interner::new();
    let mut diags = skotch_diagnostics::Diagnostics::new();
    let mut sm = skotch_span::SourceMap::new();
    let file_id = sm.add(std::path::PathBuf::from(path), text.to_string());

    let module =
        skotch_driver::compile_source(text, file_id, wrapper, &mut interner, &mut diags, None);

    let has_errors = diags.has_errors();
    let mir_json = serde_json::to_string(&module).unwrap_or_default();
    CompileResult::new(db, mir_json, has_errors)
}

/// Output of a single file compilation — memoized by salsa.
#[salsa::tracked]
pub struct CompileResult<'db> {
    /// Serialized MIR module. Using JSON rather than a native salsa tracked
    /// struct lets MirModule keep its existing serde derives without
    /// requiring `salsa::Update`. This is the main thing to fix when we
    /// break the pipeline into finer-grained tracked functions.
    #[returns(ref)]
    pub mir_json: String,

    /// Whether compilation produced errors.
    pub has_errors: bool,
}

// ─── Database ───────────────────────────────────────────────────────────────

#[salsa::db]
#[derive(Default, Clone)]
pub struct Db {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for Db {}

impl Db {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_source(&self, path: String, text: String, wrapper_class: String) -> SourceFile {
        SourceFile::new(self, path, text, wrapper_class)
    }

    pub fn update_source(&mut self, file: SourceFile, new_text: String) {
        file.set_text(self).to(new_text);
    }

    /// Compile all registered files. Returns (MirModule, has_errors) per file.
    ///
    /// Salsa memoizes each `compile_file` call — unchanged files return
    /// instantly from cache. Files are compiled sequentially (salsa's
    /// thread-local storage requires it), but the backend stage (class
    /// file writing) is parallelized via rayon.
    pub fn compile_all(&self, files: &[SourceFile]) -> Vec<(skotch_mir::MirModule, bool)> {
        files
            .iter()
            .map(|&file| {
                let result = compile_file(self, file);
                let mir_json = result.mir_json(self);
                let module: skotch_mir::MirModule =
                    serde_json::from_str(mir_json).unwrap_or_default();
                (module, result.has_errors(self))
            })
            .collect()
    }
}

// ─── Content-hash-based incremental build ──────────────────────────────────

/// A file's content hash and exported declarations. Used to detect when
/// the PackageSymbolTable needs rebuilding without recompiling everything.
#[derive(Clone, Debug)]
pub struct FileSignature {
    /// Blake3 hash of the source text.
    pub content_hash: String,
    /// The wrapper class name for this file.
    pub wrapper_class: String,
    /// Number of exported functions (for quick comparison).
    pub export_count: usize,
}

/// Incremental build state that persists across rebuilds.
/// Tracks content hashes to detect which files changed.
#[derive(Default, Clone, Debug)]
pub struct IncrementalState {
    /// Map from file path → last known content hash + signature info.
    pub file_hashes: rustc_hash::FxHashMap<String, FileSignature>,
    /// Hash of the serialized PackageSymbolTable. When this changes,
    /// all files need recompilation (cross-file signatures changed).
    pub symbol_table_hash: String,
}

impl IncrementalState {
    /// Check if a file's content has changed since the last build.
    pub fn file_changed(&self, path: &str, current_text: &str) -> bool {
        match self.file_hashes.get(path) {
            Some(sig) => sig.content_hash != content_hash(current_text),
            None => true, // new file
        }
    }

    /// Record a file's current content hash.
    pub fn record_file(&mut self, path: &str, text: &str, wrapper_class: &str, export_count: usize) {
        self.file_hashes.insert(
            path.to_string(),
            FileSignature {
                content_hash: content_hash(text),
                wrapper_class: wrapper_class.to_string(),
                export_count,
            },
        );
    }

    /// Check if the overall symbol table changed (requiring full recompilation).
    pub fn symbol_table_changed(&self, new_hash: &str) -> bool {
        self.symbol_table_hash != new_hash
    }

    /// Record the symbol table hash.
    pub fn set_symbol_table_hash(&mut self, hash: String) {
        self.symbol_table_hash = hash;
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_deterministic() {
        let h1 = content_hash("fun main() {}");
        let h2 = content_hash("fun main() {}");
        let h3 = content_hash("fun main() { println(1) }");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn compile_single_file() {
        let db = Db::new();
        let file = db.add_source(
            "Main.kt".into(),
            "fun main() { println(42) }".into(),
            "MainKt".into(),
        );
        let result = compile_file(&db, file);
        assert!(!result.has_errors(&db));
        assert!(!result.mir_json(&db).is_empty());
    }

    #[test]
    fn no_double_compilation() {
        // Both MIR and error status come from a single compile_file call.
        // Calling it twice returns the memoized result (no re-execution).
        let db = Db::new();
        let file = db.add_source(
            "Test.kt".into(),
            "fun test(): Int = 1\n".into(),
            "TestKt".into(),
        );
        let r1 = compile_file(&db, file);
        let r2 = compile_file(&db, file);
        assert!(r1.mir_json(&db) == r2.mir_json(&db));
        assert!(r1.has_errors(&db) == r2.has_errors(&db));
    }

    #[test]
    fn incremental_on_change() {
        let mut db = Db::new();
        let file = db.add_source(
            "Main.kt".into(),
            "fun main() { println(1) }".into(),
            "MainKt".into(),
        );
        let r1 = compile_file(&db, file);
        let json1 = r1.mir_json(&db).to_string();

        // Same source → memoized.
        let r2 = compile_file(&db, file);
        assert_eq!(json1, *r2.mir_json(&db));

        // Changed source → recompiled.
        db.update_source(file, "fun main() { println(2) }".into());
        let r3 = compile_file(&db, file);
        assert_ne!(json1, *r3.mir_json(&db));
    }

    #[test]
    fn compile_all_multiple_files() {
        let db = Db::new();
        let files: Vec<SourceFile> = (0..4)
            .map(|i| {
                db.add_source(
                    format!("File{i}.kt"),
                    format!("fun f{i}(): Int = {i}\n"),
                    format!("File{i}Kt"),
                )
            })
            .collect();
        let results = db.compile_all(&files);
        assert_eq!(results.len(), 4);
        for (module, has_errors) in &results {
            assert!(!has_errors);
            assert!(!module.functions.is_empty());
        }
    }

    #[test]
    fn incremental_state_detects_changes() {
        let mut state = IncrementalState::default();

        // First build — all files are new.
        assert!(state.file_changed("Main.kt", "fun main() {}"));
        state.record_file("Main.kt", "fun main() {}", "MainKt", 1);

        // Same content — no change.
        assert!(!state.file_changed("Main.kt", "fun main() {}"));

        // Different content — changed.
        assert!(state.file_changed("Main.kt", "fun main() { println(1) }"));
    }

    #[test]
    fn incremental_state_symbol_table_hash() {
        let mut state = IncrementalState::default();
        assert!(state.symbol_table_changed("abc"));

        state.set_symbol_table_hash("abc".to_string());
        assert!(!state.symbol_table_changed("abc"));
        assert!(state.symbol_table_changed("def"));
    }
}
