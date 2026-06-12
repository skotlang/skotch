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
    let typed = skotch_typeck::typed::type_check(file, &resolved, interner, diags, package_symbols);
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

    #[test]
    fn typed_compile_source_handles_top_level_val_and_fn() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "val MAX: Int = 100\nfun overflow(x: Int): Boolean = x > MAX",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        // Confirm at least one fn made it through.
        assert!(module.functions.iter().any(|f| f.name == "overflow"));
    }

    #[test]
    fn typed_compile_source_handles_class_with_method() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "class P(val n: Int) { fun double(): Int = n * 2 }",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let cls = module.classes.iter().find(|c| c.name == "P");
        assert!(cls.is_some(), "expected class P");
        let cls = cls.unwrap();
        assert!(cls.methods.iter().any(|m| m.name == "double"));
    }

    #[test]
    fn typed_compile_source_handles_class_with_field_method() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            r#"class P(val name: String) {
                fun greet(): String = "Hello, $name"
                fun len(): Int = name.length
            }"#,
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let cls = module.classes.iter().find(|c| c.name == "P").unwrap();
        // The greet method should exist.
        assert!(cls.methods.iter().any(|m| m.name == "greet"));
    }

    #[test]
    fn typed_compile_source_handles_block_with_var_reassign() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "fun acc(): Int { var sum = 0; sum = sum + 1; return sum }",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "acc").unwrap();
        // 2 Const Assigns (0, 1) + 1 AddI = 3 stmts; ReturnValue terminator.
        assert!(matches!(
            f.blocks[0].terminator,
            skotch_mir::Terminator::ReturnValue(_)
        ));
    }

    #[test]
    fn typed_compile_source_handles_throw_inline_ctor() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "fun fail(): Nothing = throw IllegalStateException()",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "fail").unwrap();
        assert!(matches!(
            f.blocks[0].terminator,
            skotch_mir::Terminator::Throw(_)
        ));
    }

    #[test]
    fn typed_compile_source_handles_class_instantiation() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "class P(val x: Int)\nfun mk(): P = P(42)",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "mk").unwrap();
        // Body should have NewInstance + Constructor call.
        let has_new = f.blocks[0].stmts.iter().any(|s| {
            matches!(
                s,
                skotch_mir::Stmt::Assign {
                    value: skotch_mir::Rvalue::NewInstance(_),
                    ..
                }
            )
        });
        let has_ctor = f.blocks[0].stmts.iter().any(|s| {
            matches!(
                s,
                skotch_mir::Stmt::Assign {
                    value: skotch_mir::Rvalue::Call {
                        kind: skotch_mir::CallKind::Constructor(_),
                        ..
                    },
                    ..
                }
            )
        });
        assert!(has_new);
        assert!(has_ctor);
    }

    #[test]
    fn typed_compile_source_handles_when_expression() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            r#"fun lookup(x: Int): String = when (x) { 1 -> "one"; 2 -> "two"; else -> "other" }"#,
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module
            .functions
            .iter()
            .find(|f| f.name == "lookup")
            .unwrap();
        // 2 arms × 2 blocks + 1 else + 1 join = 6 blocks.
        assert_eq!(f.blocks.len(), 6);
    }

    #[test]
    fn typed_compile_source_handles_try_catch() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "fun parse(): Int = try { 1 } catch (e: Exception) { 0 }",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "parse").unwrap();
        assert_eq!(f.blocks.len(), 3);
        assert_eq!(f.exception_handlers.len(), 1);
    }

    #[test]
    fn typed_compile_source_handles_string_template() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            r#"fun greet(name: String): String = "Hello, $name""#,
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "greet").unwrap();
        // Should contain MakeConcatWithConstants Call.
        let has_concat = f.blocks[0].stmts.iter().any(|s| {
            matches!(
                s,
                skotch_mir::Stmt::Assign {
                    value: skotch_mir::Rvalue::Call {
                        kind: skotch_mir::CallKind::MakeConcatWithConstants { .. },
                        ..
                    },
                    ..
                }
            )
        });
        assert!(has_concat);
    }

    #[test]
    fn typed_compile_source_handles_if_else() {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let module = compile_source(
            "fun absVal(x: Int): Int = if (x < 0) -x else x",
            "test.kt",
            "TestKt",
            &mut interner,
            &mut diags,
            None,
        );
        let f = module.functions.iter().find(|f| f.name == "absVal");
        assert!(f.is_some());
        // 4-block CFG for if/else.
        assert_eq!(f.unwrap().blocks.len(), 4);
    }
}
