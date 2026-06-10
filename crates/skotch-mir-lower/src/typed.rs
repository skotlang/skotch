//! Typed-AST entry point for MIR lowering.
//!
//! Parallel to the legacy [`crate::lower_file`] but takes a
//! [`skotch_ast::KtFile`] (typed view over the SIL tree) instead of
//! the Box-tree `&skotch_syntax::KtFile`.
//!
//! ## Current coverage
//!
//! Initial scaffold returns an empty [`skotch_mir::MirModule`] with
//! the wrapper class name populated. Each consumer migration step
//! expands the coverage one decl/expression form at a time. Same
//! migration pattern as [`skotch_resolve::typed`] and
//! [`skotch_typeck::typed`].

use skotch_ast::KtFile;
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::MirModule;
use skotch_resolve::{PackageSymbolTable, ResolvedFile};
use skotch_typeck::TypedFile;

/// Lower a single typed file to MIR.
///
/// Counterpart of [`crate::lower_file`]. The returned [`MirModule`]
/// is a scaffold for the initial pass; once the migration completes,
/// the full coverage matches the legacy lowerer's output.
pub fn lower_file(
    _file: KtFile<'_>,
    _resolved: &ResolvedFile,
    _typed: &TypedFile,
    _interner: &mut Interner,
    _diags: &mut Diagnostics,
    wrapper_class: &str,
    _package_symbols: Option<&PackageSymbolTable>,
) -> MirModule {
    // Scaffold — populated in the per-consumer migration steps that
    // exercise mir-lower against typed AST input. The body of the
    // legacy `lower_file` is ~27k lines of pattern-matching across
    // Decl, Expr, Stmt, Block, Param, TypeRef, ConstructorParam,
    // EnumEntry, SecondaryConstructor; that body is what the typed
    // migration must port one composite kind at a time.
    MirModule {
        wrapper_class: wrapper_class.to_string(),
        ..MirModule::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_lower_file_scaffold_returns_wrapper() {
        let parsed = skotch_ast::parse("test.kt", "fun main() {}");
        let resolved = ResolvedFile::default();
        let typed = TypedFile::default();
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = lower_file(
            parsed.file(),
            &resolved,
            &typed,
            &mut interner,
            &mut diags,
            "TestKt",
            None,
        );
        assert_eq!(module.wrapper_class, "TestKt");
    }
}
