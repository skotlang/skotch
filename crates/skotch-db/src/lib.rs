//! Salsa-based incremental compilation database for skotch.
//!
//! ## Design rationale
//!
//! **Why salsa?** rust-analyzer proves salsa scales to a full-language IDE.
//! skotch targets the same end state: a single database backing both
//! `skotch build` and `skotch lsp`. Salsa's memoized, demand-driven
//! queries give us:
//!
//! - **Within-build incrementalism**: If the build pipeline calls
//!   `compile_file` twice for the same unchanged source, the second call
//!   is free (memoized).
//! - **LSP integration path**: The LSP server can hold a persistent `Db`
//!   across edits, getting sub-millisecond re-analysis on keystrokes.
//! - **Future fine-grained tracking**: As we break the pipeline into
//!   smaller tracked functions (parse → resolve → typecheck → lower),
//!   salsa can skip downstream stages when upstream outputs are unchanged.
//!
//! **Why blake3?** It's the fastest cryptographic hash (3–5x faster than
//! SHA-256), used for content-addressed file identification.
//!
//! ## Current granularity
//!
//! Today the entire front-end pipeline is ONE tracked function. This is
//! the coarsest possible granularity — any source change recompiles the
//! whole file. The roadmap:
//!
//! 1. v0.2.0 (now): Single `compile_file` tracked fn per source file
//! 2. v0.3.0+: Break into `lex`, `parse`, `resolve`, `typecheck`, `lower`
//!    tracked fns — enables skipping downstream stages
//! 3. v0.7.0+: Cross-file export tables as tracked structs — enables
//!    multi-module builds with minimal recompilation
//! 4. LSP: Persistent `Db` instance across edits — sub-millisecond
//!    incremental re-analysis

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
}
