use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;

#[test]
fn probe_589() {
    let src = std::fs::read_to_string("../../tests/fixtures/inputs/589-suspend-int-from-int-param/input.kt").unwrap();
    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let typed = skotch_driver::typed::compile_source(&src, "input.kt", "InputKt", &mut interner, &mut diags, None);
    eprintln!("=== TYPED 589 ===");
    for f in &typed.functions {
        eprintln!("fn {}: blocks={}, is_suspend={}", f.name, f.blocks.len(), f.is_suspend);
        for (i, b) in f.blocks.iter().enumerate() {
            eprintln!("  blk{}: {} stmts, term={:?}", i, b.stmts.len(), b.terminator);
        }
    }
    let mut interner = Interner::new();
    let mut diags = Diagnostics::new();
    let legacy = skotch_driver::compile_source(&src, skotch_span::FileId(0), "InputKt", &mut interner, &mut diags, None);
    eprintln!("=== LEGACY 589 ===");
    for f in &legacy.functions {
        eprintln!("fn {}: blocks={}, is_suspend={}", f.name, f.blocks.len(), f.is_suspend);
        for (i, b) in f.blocks.iter().enumerate() {
            eprintln!("  blk{}: {} stmts, term={:?}", i, b.stmts.len(), b.terminator);
            for s in &b.stmts { eprintln!("    {:?}", s); }
        }
    }
}
