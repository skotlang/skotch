//! Parity tests between the legacy `gather_declarations` /
//! `resolve_file` and their typed-AST counterparts in `skotch_resolve::typed`.
//!
//! The legacy implementation consumes the Box-tree `skotch_syntax::KtFile`
//! produced by `skotch_parser::parse_file`. The typed implementation
//! consumes `skotch_ast::KtFile` (typed view over the SIL tree).
//! Both must produce semantically identical `PackageSymbolTable` /
//! `ResolvedFile` output for the migration to be safe.
//!
//! Each test fans both pipelines through the same source and asserts
//! shape-equality on the cross-file canonical fields. Map iteration is
//! non-deterministic, so we sort keys before diffing.

use skotch_intern::Interner;
use skotch_lexer::lex;
use skotch_parser::parse_file;
use skotch_resolve::{
    gather_declarations as legacy_gather, typed::gather_declarations as typed_gather,
    ExternalClassDecl, ExternalClassKind, PackageSymbolTable,
};
use skotch_span::FileId;

fn legacy_parse(src: &str) -> (skotch_syntax::KtFile, Interner) {
    let mut interner = Interner::new();
    let mut diags = skotch_diagnostics::Diagnostics::default();
    let lexed = lex(FileId(0), src, &mut diags);
    let ast = parse_file(&lexed, &mut interner, &mut diags);
    (ast, interner)
}

fn run_both(src: &str) -> (PackageSymbolTable, PackageSymbolTable) {
    let (legacy_ast, legacy_interner) = legacy_parse(src);
    let legacy_table = legacy_gather(&[(FileId(0), &legacy_ast, "TestKt")], &legacy_interner);

    let typed = skotch_ast::parse("test.kt", src);
    let typed_interner = Interner::new();
    let typed_table = typed_gather(&[(typed.file(), "TestKt")], &typed_interner);

    (legacy_table, typed_table)
}

fn sorted_keys(t: &PackageSymbolTable) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut fns: Vec<String> = t.functions.keys().cloned().collect();
    let mut vals: Vec<String> = t.vals.keys().cloned().collect();
    let mut classes: Vec<String> = t.classes.keys().cloned().collect();
    fns.sort();
    vals.sort();
    classes.sort();
    (fns, vals, classes)
}

fn assert_top_level_keys_match(legacy: &PackageSymbolTable, typed: &PackageSymbolTable) {
    let (lf, lv, lc) = sorted_keys(legacy);
    let (tf, tv, tc) = sorted_keys(typed);
    assert_eq!(lf, tf, "function-name key set differs");
    assert_eq!(lv, tv, "val-name key set differs");
    assert_eq!(lc, tc, "class-name key set differs");
}

fn assert_class_shape_match(
    legacy: &ExternalClassDecl,
    typed: &ExternalClassDecl,
    name: &str,
) {
    assert_eq!(legacy.jvm_name, typed.jvm_name, "{name}: jvm_name");
    assert_eq!(legacy.kind, typed.kind, "{name}: kind");
    assert_eq!(legacy.fields.len(), typed.fields.len(), "{name}: field count");
    // Field name + type equality, in order.
    for ((ln, lt), (tn, tt)) in legacy.fields.iter().zip(typed.fields.iter()) {
        assert_eq!(ln, tn, "{name}: field name");
        assert_eq!(lt, tt, "{name}: field {ln} ty");
    }
    assert_eq!(legacy.is_open, typed.is_open, "{name}: is_open");
    assert_eq!(legacy.is_abstract, typed.is_abstract, "{name}: is_abstract");
    assert_eq!(legacy.is_inner, typed.is_inner, "{name}: is_inner");
    assert_eq!(legacy.super_class, typed.super_class, "{name}: super_class");
    assert_eq!(
        sorted(&legacy.interfaces),
        sorted(&typed.interfaces),
        "{name}: interfaces"
    );
    assert_eq!(
        sorted(&legacy.enum_entries),
        sorted(&typed.enum_entries),
        "{name}: enum_entries"
    );
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut s = v.to_vec();
    s.sort();
    s
}

#[test]
fn parity_empty_main() {
    let (l, t) = run_both("fun main() {}");
    assert_top_level_keys_match(&l, &t);
}

#[test]
fn parity_top_level_fun_descriptor() {
    let (l, t) = run_both("fun add(a: Int, b: Int): Int = a + b");
    assert_top_level_keys_match(&l, &t);
    let lf = &l.functions["add"][0];
    let tf = &t.functions["add"][0];
    assert_eq!(lf.descriptor, tf.descriptor);
    assert_eq!(lf.return_ty, tf.return_ty);
    assert_eq!(lf.param_tys, tf.param_tys);
    assert_eq!(lf.param_count, tf.param_count);
    assert_eq!(lf.owner_class, tf.owner_class);
}

#[test]
fn parity_class_with_primary_ctor() {
    let (l, t) = run_both("class P(val x: Int, val y: String)");
    let lc = l.classes.get("P").expect("legacy P");
    let tc = t.classes.get("P").expect("typed P");
    assert_class_shape_match(lc, tc, "P");
}

#[test]
fn parity_data_class() {
    let (l, t) = run_both("data class Point(val x: Int, val y: Int)");
    let lc = l.classes.get("Point").expect("legacy Point");
    let tc = t.classes.get("Point").expect("typed Point");
    assert_class_shape_match(lc, tc, "Point");
    assert_eq!(lc.kind, ExternalClassKind::DataClass);
    assert_eq!(tc.kind, ExternalClassKind::DataClass);
}

#[test]
fn parity_enum_class() {
    let (l, t) = run_both("enum class Color { RED, GREEN, BLUE }");
    let lc = l.classes.get("Color").expect("legacy");
    let tc = t.classes.get("Color").expect("typed");
    assert_class_shape_match(lc, tc, "Color");
}

#[test]
fn parity_interface() {
    let (l, t) = run_both("interface Printable { fun pretty(): String }");
    let lc = l.classes.get("Printable").expect("legacy");
    let tc = t.classes.get("Printable").expect("typed");
    assert_class_shape_match(lc, tc, "Printable");
}

#[test]
fn parity_object_singleton() {
    let (l, t) = run_both("object S { fun greet(): String = \"hi\" }");
    let lc = l.classes.get("S").expect("legacy");
    let tc = t.classes.get("S").expect("typed");
    assert_class_shape_match(lc, tc, "S");
}

#[test]
fn parity_typealias_descriptor() {
    let (l, t) = run_both(
        "typealias Predicate = (Int) -> Boolean\nfun apply(p: Predicate): Boolean = true",
    );
    assert_eq!(
        l.functions["apply"][0].descriptor,
        t.functions["apply"][0].descriptor,
        "typealias must substitute in descriptor"
    );
}

#[test]
fn parity_extension_function() {
    let (l, t) = run_both("fun String.exclaim(): String = this + \"!\"");
    let lf = &l.functions["exclaim"][0];
    let tf = &t.functions["exclaim"][0];
    assert_eq!(lf.is_extension, tf.is_extension);
    assert_eq!(lf.receiver_ty, tf.receiver_ty);
}

#[test]
fn parity_nullable_descriptor() {
    let (l, t) = run_both("fun maybe(x: String?): String? = null");
    assert_eq!(
        l.functions["maybe"][0].descriptor,
        t.functions["maybe"][0].descriptor
    );
}

#[test]
fn parity_top_level_val() {
    let (l, t) = run_both("val GREETING: String = \"hi\"");
    assert!(l.vals.contains_key("GREETING"));
    assert!(t.vals.contains_key("GREETING"));
    assert_eq!(l.vals["GREETING"].ty, t.vals["GREETING"].ty);
}

#[test]
fn parity_package_prefix() {
    let (l, t) = run_both("package com.foo\nclass Bar");
    let lc = l.classes.get("Bar").expect("legacy Bar");
    let tc = t.classes.get("Bar").expect("typed Bar");
    assert_eq!(lc.jvm_name, "com/foo/Bar");
    assert_eq!(tc.jvm_name, "com/foo/Bar");
}

#[test]
fn parity_nested_class_outer_inner_jvm_name() {
    let (l, t) = run_both("class Outer { class Inner }");
    let li = l.classes_by_fq.get("Outer$Inner").expect("legacy nested");
    let ti = t.classes_by_fq.get("Outer$Inner").expect("typed nested");
    assert_eq!(li.jvm_name, ti.jvm_name);
}
