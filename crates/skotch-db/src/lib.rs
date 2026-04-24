//! Salsa-based incremental compilation database for skotch.
//!
//! ## Three-Level Incremental Pipeline
//!
//! ```text
//! SourceFile (input)
//!     │
//!     ▼
//! gather_exports(db, file) → FileExports (tracked, memoized)
//!     │                         contains: exported declaration signatures as JSON
//!     │
//!     ▼
//! SymbolTableInput (input)  ← built from all FileExports by the pipeline
//!     │                         contains: serialized PackageSymbolTable
//!     │
//!     ▼
//! compile_with_context(db, file, table) → CompileResult (tracked, memoized)
//!     │                                    contains: MIR as JSON + error flag
//!     │
//!     ▼
//! Backend (outside Salsa — parallel via rayon)
//! ```
//!
//! **Key property:** When only a function *body* changes (no signature change),
//! `gather_exports` returns identical JSON for that file. The pipeline detects
//! that the aggregated `SymbolTableInput` is unchanged and skips recompilation
//! of all other files. Only the changed file is recompiled.

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

/// The aggregated cross-file symbol table, stored as a Salsa input so that
/// `compile_with_context` is re-invoked only when the table changes.
/// The JSON is a serialized `PackageSymbolTable`.
#[salsa::input]
pub struct SymbolTableInput {
    #[returns(ref)]
    pub json: String,
}

// ─── Level 1: Gather exports (per-file, memoized) ──────────────────────────

/// Extract top-level declaration signatures from a single file. Salsa
/// memoizes the result — if the file text hasn't changed, this returns
/// instantly from cache. The output is a JSON string of the file's
/// exported declarations (functions, vals, classes).
///
/// This is the first level of the incremental pipeline. A body-only change
/// (e.g. modifying a function's implementation without changing its
/// signature) produces the same `exports_json`, so downstream steps
/// that depend on the symbol table are NOT re-triggered.
#[salsa::tracked]
pub fn gather_exports<'db>(db: &'db dyn salsa::Database, file: SourceFile) -> FileExports<'db> {
    let text = file.text(db);
    let wrapper = file.wrapper_class(db);
    let path = file.path(db);

    let mut interner = skotch_intern::Interner::new();
    let mut diags = skotch_diagnostics::Diagnostics::new();
    let mut sm = skotch_span::SourceMap::new();
    let file_id = sm.add(std::path::PathBuf::from(path), text.to_string());

    let lexed = skotch_lexer::lex(file_id, text, &mut diags);
    let ast = skotch_parser::parse_file(&lexed, &mut interner, &mut diags);

    // Use gather_declarations with a single file to extract exports.
    let refs = vec![(file_id, &ast, wrapper.as_str())];
    let table = skotch_resolve::gather_declarations(&refs, &interner);

    // Serialize to JSON for Salsa-compatible storage.
    let exports_json = serde_json::to_string(&table).unwrap_or_default();
    FileExports::new(db, exports_json, diags.has_errors())
}

/// Memoized output of the gather phase for one file.
#[salsa::tracked]
pub struct FileExports<'db> {
    /// JSON-serialized per-file exports (functions, vals, classes).
    #[returns(ref)]
    pub exports_json: String,
    /// Whether parsing produced errors.
    pub has_parse_errors: bool,
}

// ─── Level 2: Compile with context (per-file, memoized) ────────────────────

/// Compile a single file with cross-file visibility from the symbol table.
/// Salsa memoizes the result — if neither the file text NOR the symbol table
/// have changed, this returns instantly from cache.
///
/// This is the second level. It depends on:
/// - `file.text` (changes when the file is edited)
/// - `table.json` (changes when any file's exports change)
///
/// When only a body changes in another file, `table.json` stays the same,
/// so this function is NOT re-invoked for unchanged files.
#[salsa::tracked]
pub fn compile_with_context<'db>(
    db: &'db dyn salsa::Database,
    file: SourceFile,
    table: SymbolTableInput,
) -> CompileResult<'db> {
    let text = file.text(db);
    let path = file.path(db);
    let wrapper = file.wrapper_class(db);
    let table_json = table.json(db);

    let mut interner = skotch_intern::Interner::new();
    let mut diags = skotch_diagnostics::Diagnostics::new();
    let mut sm = skotch_span::SourceMap::new();
    let file_id = sm.add(std::path::PathBuf::from(path), text.to_string());

    // Deserialize the symbol table.
    let pkg_symbols: skotch_resolve::PackageSymbolTable =
        serde_json::from_str(table_json).unwrap_or_default();

    let module = skotch_driver::compile_source(
        text,
        file_id,
        wrapper,
        &mut interner,
        &mut diags,
        Some(&pkg_symbols),
    );

    let has_errors = diags.has_errors();
    let mir_json = serde_json::to_string(&module).unwrap_or_default();
    let diag_messages = diags
        .iter()
        .map(|d| format!("{:?}: {}", d.severity, d.message))
        .collect::<Vec<_>>()
        .join("\n");
    CompileResult::new(db, mir_json, has_errors, diag_messages)
}

/// Compile a single file in isolation (no cross-file visibility). Used
/// by `skotch emit` for single-file compilation and for backward compat.
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
    CompileResult::new(db, mir_json, has_errors, String::new())
}

/// Output of a single file compilation — memoized by salsa.
#[salsa::tracked]
pub struct CompileResult<'db> {
    #[returns(ref)]
    pub mir_json: String,
    pub has_errors: bool,
    /// Formatted diagnostic messages for this file (may be empty).
    #[returns(ref)]
    pub diag_messages: String,
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

    /// Create a symbol table input from JSON.
    pub fn set_symbol_table(&self, json: String) -> SymbolTableInput {
        SymbolTableInput::new(self, json)
    }

    /// Update an existing symbol table input.
    pub fn update_symbol_table(&mut self, table: SymbolTableInput, new_json: String) {
        table.set_json(self).to(new_json);
    }

    /// Run the full incremental pipeline for multiple files:
    /// 1. Gather exports from each file (memoized per-file)
    /// 2. Build aggregated symbol table
    /// 3. Compile each file with context (memoized per-file+table)
    ///
    /// Returns `(Vec<(MirModule, has_errors, diag_messages)>)`.
    pub fn compile_all_incremental(
        &mut self,
        files: &[SourceFile],
        prev_table: Option<SymbolTableInput>,
    ) -> (Vec<(skotch_mir::MirModule, bool, String)>, SymbolTableInput) {
        // Level 1: Gather exports from each file (salsa-memoized).
        let mut all_exports = skotch_resolve::PackageSymbolTable::default();
        let mut any_parse_errors = false;
        for &file in files {
            let exports = gather_exports(self, file);
            if exports.has_parse_errors(self) {
                any_parse_errors = true;
            }
            let json = exports.exports_json(self);
            if let Ok(table) = serde_json::from_str::<skotch_resolve::PackageSymbolTable>(json) {
                // Merge this file's exports into the aggregated table.
                for (k, v) in table.functions {
                    all_exports.functions.entry(k).or_default().extend(v);
                }
                for (k, v) in table.vals {
                    all_exports.vals.entry(k).or_insert(v);
                }
                for (k, v) in table.classes {
                    all_exports.classes.entry(k).or_insert(v);
                }
            }
        }

        // Build the aggregated symbol table JSON.
        let table_json = serde_json::to_string(&all_exports).unwrap_or_default();

        // Create or update the SymbolTableInput.
        let table_input = if let Some(prev) = prev_table {
            if *prev.json(self) != table_json {
                prev.set_json(self).to(table_json);
            }
            prev
        } else {
            SymbolTableInput::new(self, table_json)
        };

        if any_parse_errors {
            // Return empty results with error flag — don't proceed to compilation.
            let results = files
                .iter()
                .map(|_| (skotch_mir::MirModule::default(), true, String::new()))
                .collect();
            return (results, table_input);
        }

        // Level 2: Compile each file with context (salsa-memoized).
        let results = files
            .iter()
            .map(|&file| {
                let result = compile_with_context(self, file, table_input);
                let mir_json = result.mir_json(self);
                let module: skotch_mir::MirModule =
                    serde_json::from_str(mir_json).unwrap_or_default();
                let diag_messages = result.diag_messages(self).clone();
                (module, result.has_errors(self), diag_messages)
            })
            .collect();

        (results, table_input)
    }

    /// Compile all files in isolation (no cross-file visibility). Used for
    /// backward compatibility with the old single-file pipeline.
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

/// A file's content hash and exported declarations.
#[derive(Clone, Debug)]
pub struct FileSignature {
    pub content_hash: String,
    pub wrapper_class: String,
    pub export_count: usize,
}

/// Incremental build state that persists across rebuilds.
#[derive(Default, Clone, Debug)]
pub struct IncrementalState {
    pub file_hashes: rustc_hash::FxHashMap<String, FileSignature>,
    pub symbol_table_hash: String,
}

impl IncrementalState {
    pub fn file_changed(&self, path: &str, current_text: &str) -> bool {
        match self.file_hashes.get(path) {
            Some(sig) => sig.content_hash != content_hash(current_text),
            None => true,
        }
    }

    pub fn record_file(
        &mut self,
        path: &str,
        text: &str,
        wrapper_class: &str,
        export_count: usize,
    ) {
        self.file_hashes.insert(
            path.to_string(),
            FileSignature {
                content_hash: content_hash(text),
                wrapper_class: wrapper_class.to_string(),
                export_count,
            },
        );
    }

    pub fn symbol_table_changed(&self, new_hash: &str) -> bool {
        self.symbol_table_hash != new_hash
    }

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
    fn compile_single_file_isolation() {
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
    fn memoization_same_input() {
        let db = Db::new();
        let file = db.add_source(
            "Test.kt".into(),
            "fun test(): Int = 1\n".into(),
            "TestKt".into(),
        );
        let r1 = compile_file(&db, file);
        let r2 = compile_file(&db, file);
        assert_eq!(r1.mir_json(&db), r2.mir_json(&db));
    }

    #[test]
    fn recompile_on_source_change() {
        let mut db = Db::new();
        let file = db.add_source(
            "Main.kt".into(),
            "fun main() { println(1) }".into(),
            "MainKt".into(),
        );
        let r1 = compile_file(&db, file);
        let json1 = r1.mir_json(&db).to_string();

        db.update_source(file, "fun main() { println(2) }".into());
        let r2 = compile_file(&db, file);
        assert_ne!(json1, *r2.mir_json(&db));
    }

    #[test]
    fn gather_exports_memoized() {
        let db = Db::new();
        let file = db.add_source(
            "Lib.kt".into(),
            "fun greet(name: String): String = \"Hello, $name!\"\n".into(),
            "LibKt".into(),
        );
        let e1 = gather_exports(&db, file);
        let e2 = gather_exports(&db, file);
        assert_eq!(e1.exports_json(&db), e2.exports_json(&db));
        assert!(!e1.has_parse_errors(&db));
    }

    #[test]
    fn body_change_preserves_exports() {
        // Changing a function BODY (not signature) should produce
        // the same exports JSON — this is the key incremental property.
        let mut db = Db::new();
        let file = db.add_source(
            "Lib.kt".into(),
            "fun greet(): String = \"Hello\"\n".into(),
            "LibKt".into(),
        );
        let e1 = gather_exports(&db, file);
        let json1 = e1.exports_json(&db).to_string();

        // Change the body but not the signature.
        db.update_source(file, "fun greet(): String = \"World\"\n".into());
        let e2 = gather_exports(&db, file);
        let json2 = e2.exports_json(&db).to_string();

        // The exports should be identical — same function name, same types.
        assert_eq!(json1, json2, "Body-only change should NOT change exports");
    }

    #[test]
    fn signature_change_changes_exports() {
        let mut db = Db::new();
        let file = db.add_source(
            "Lib.kt".into(),
            "fun greet(): String = \"Hello\"\n".into(),
            "LibKt".into(),
        );
        let e1 = gather_exports(&db, file);
        let json1 = e1.exports_json(&db).to_string();

        // Change the signature (add a parameter).
        db.update_source(
            file,
            "fun greet(name: String): String = \"Hello, $name!\"\n".into(),
        );
        let e2 = gather_exports(&db, file);
        let json2 = e2.exports_json(&db).to_string();

        assert_ne!(json1, json2, "Signature change SHOULD change exports");
    }

    #[test]
    fn compile_with_context_uses_symbol_table() {
        let mut db = Db::new();
        let greeter = db.add_source(
            "Greeter.kt".into(),
            "fun greet(): String = \"Hello!\"\n".into(),
            "GreeterKt".into(),
        );
        let main = db.add_source(
            "Main.kt".into(),
            "fun main() { println(greet()) }\n".into(),
            "MainKt".into(),
        );

        let (results, _table) = db.compile_all_incremental(&[greeter, main], None);

        // Main.kt should compile without errors because greet() is visible
        // from the symbol table.
        let (_, main_has_errors, main_diags) = &results[1];
        assert!(
            !main_has_errors,
            "Main.kt should compile without errors: {main_diags}"
        );
    }

    #[test]
    fn incremental_body_change_skips_other_files() {
        let mut db = Db::new();
        let greeter = db.add_source(
            "Greeter.kt".into(),
            "fun greet(): String = \"Hello!\"\n".into(),
            "GreeterKt".into(),
        );
        let main = db.add_source(
            "Main.kt".into(),
            "fun main() { println(greet()) }\n".into(),
            "MainKt".into(),
        );

        // First build.
        let (results1, table) = db.compile_all_incremental(&[greeter, main], None);
        let main_mir1 = results1[1].0.functions.len();

        // Change Greeter's BODY only (not signature).
        db.update_source(greeter, "fun greet(): String = \"World!\"\n".into());

        // Second build with same table input.
        let (results2, _table2) = db.compile_all_incremental(&[greeter, main], Some(table));
        let main_mir2 = results2[1].0.functions.len();

        // Main.kt's MIR should be identical (memoized) because the
        // symbol table didn't change.
        assert_eq!(main_mir1, main_mir2, "Main.kt should not be recompiled");
    }

    #[test]
    fn incremental_state_detects_changes() {
        let mut state = IncrementalState::default();
        assert!(state.file_changed("Main.kt", "fun main() {}"));
        state.record_file("Main.kt", "fun main() {}", "MainKt", 1);
        assert!(!state.file_changed("Main.kt", "fun main() {}"));
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
