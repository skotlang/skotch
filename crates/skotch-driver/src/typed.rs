//! Typed-AST compilation pipeline.
//!
//! End-to-end source-to-MIR compilation routed through
//! [`skotch_ast::parse`] (SIL grammar) and the `typed` modules of
//! resolve / typeck / mir-lower. Mirrors [`crate::compile_source`]
//! but the entire pipeline reads typed AST instead of the legacy
//! Box-tree `KtFile`.

use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::MirModule;
use skotch_resolve::{typed::resolve_file, PackageSymbolTable};

/// Compile source to a [`MirModule`] using the typed-AST pipeline.
///
/// Counterpart of [`crate::compile_source`]; once consumer migration
/// completes the legacy entry point becomes a thin shim around this.
pub fn compile_source(
    source: &str,
    file_name: &str,
    wrapper_class: &str,
    interner: &mut Interner,
    diags: &mut Diagnostics,
    package_symbols: Option<&PackageSymbolTable>,
) -> MirModule {
    let parsed = skotch_ast::parse(file_name, source);
    let file = parsed.file();
    let resolved = resolve_file(file, interner, package_symbols);
    let typed = skotch_typeck::typed::type_check(
        file,
        &resolved,
        interner,
        diags,
        package_symbols,
    );
    skotch_mir_lower::typed::lower_file(
        file,
        &resolved,
        &typed,
        interner,
        diags,
        wrapper_class,
        package_symbols,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_compile_source_scaffold_runs() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "fun main() {}",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        assert_eq!(module.wrapper_class, "TestKt");
    }
}
