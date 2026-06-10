//! Parity tests between the legacy `type_check` and its typed-AST
//! counterpart in `skotch_typeck::typed`. Asserts that both pipelines
//! produce the same `TypedFile` signatures for top-level declarations.
//!
//! Coverage is intentionally bounded to Pass 1 (signature collection)
//! since Pass 2 (body inference) is still being ported into the typed
//! module.

use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_lexer::lex;
use skotch_parser::parse_file;
use skotch_resolve::PackageSymbolTable;
use skotch_span::FileId;
use skotch_typeck::{type_check as legacy_check, typed::type_check as typed_check, TypedFile};

fn run_legacy(src: &str) -> (TypedFile, Interner) {
    let mut interner = Interner::new();
    let mut diags = Diagnostics::default();
    let lexed = lex(FileId(0), src, &mut diags);
    let ast = parse_file(&lexed, &mut interner, &mut diags);
    let resolved =
        skotch_resolve::resolve_file(&ast, &mut interner, &mut diags, None);
    let table = PackageSymbolTable::default();
    let typed = legacy_check(&ast, &resolved, &mut interner, &mut diags, Some(&table));
    (typed, interner)
}

fn run_typed(src: &str) -> TypedFile {
    let parsed = skotch_ast::parse("test.kt", src);
    let resolved = skotch_resolve::ResolvedFile::default();
    let mut interner = Interner::new();
    let mut diags = Diagnostics::default();
    typed_check(parsed.file(), &resolved, &mut interner, &mut diags, None)
}

#[test]
fn parity_fun_int_arith() {
    let (l, _) = run_legacy("fun add(a: Int, b: Int): Int = a + b");
    let t = run_typed("fun add(a: Int, b: Int): Int = a + b");
    assert_eq!(l.functions.len(), t.functions.len());
    assert_eq!(l.functions[0].param_tys, t.functions[0].param_tys);
    assert_eq!(l.functions[0].return_ty, t.functions[0].return_ty);
}

#[test]
fn parity_fun_string_param() {
    let (l, _) = run_legacy("fun greet(name: String): String = name");
    let t = run_typed("fun greet(name: String): String = name");
    assert_eq!(l.functions[0].param_tys, t.functions[0].param_tys);
    assert_eq!(l.functions[0].return_ty, t.functions[0].return_ty);
}

#[test]
fn parity_top_val_string() {
    let (l, _) = run_legacy("val GREETING: String = \"hi\"");
    let t = run_typed("val GREETING: String = \"hi\"");
    assert_eq!(l.top_vals.len(), t.top_vals.len());
    assert_eq!(l.top_vals[0].ty, t.top_vals[0].ty);
}

#[test]
fn parity_nullable_param() {
    let (l, _) = run_legacy("fun maybe(x: Int?): String? = null");
    let t = run_typed("fun maybe(x: Int?): String? = null");
    assert_eq!(l.functions[0].param_tys, t.functions[0].param_tys);
    assert_eq!(l.functions[0].return_ty, t.functions[0].return_ty);
}

#[test]
fn parity_unit_return() {
    let (l, _) = run_legacy("fun side() {}");
    let t = run_typed("fun side() {}");
    assert_eq!(l.functions[0].return_ty, t.functions[0].return_ty);
}
