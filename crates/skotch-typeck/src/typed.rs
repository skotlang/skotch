//! Typed-AST entry point for type checking.
//!
//! Parallel to the legacy [`crate::type_check`] but takes a
//! [`skotch_ast::KtFile`] (typed view over a SIL tree) instead of the
//! Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Initial scaffold returns an empty [`crate::TypedFile`]. Each
//! consumer migration step expands the coverage. The same migration
//! pattern as [`skotch_resolve::typed`] applies — fill in the
//! pattern-match arms as upstream callers move to the typed API.

use crate::TypedFile;
use skotch_ast::KtFile;
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_resolve::{PackageSymbolTable, ResolvedFile};

/// Type-check a single file using the typed AST input.
///
/// Counterpart of [`crate::type_check`]. The returned
/// [`TypedFile`] is a placeholder for the initial scaffold; once the
/// migration completes, the full body coverage matches the legacy
/// type-checker's output.
pub fn type_check(
    _file: KtFile<'_>,
    _resolved: &ResolvedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    _package_symbols: Option<&PackageSymbolTable>,
) -> TypedFile {
    // Scaffold — populated in the per-consumer migration steps that
    // exercise typeck against typed AST input.
    TypedFile::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_type_check_scaffold_returns_empty() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {}");
        let resolved = ResolvedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let typed = type_check(parsed.file(), &resolved, &mut interner, &mut diags, None);
        assert!(typed.functions.is_empty());
    }
}
