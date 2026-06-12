//! Typed vs legacy mir-lower coverage worklist.
//!
//! Runs both pipelines (`skotch_driver::compile_source` and
//! `skotch_driver::typed::compile_source`) over every supported
//! fixture, computes a per-fixture "completeness ratio" of the
//! typed MirModule vs the legacy one, and emits a sorted worklist
//! to `tests/fixtures/typed_worklist.txt`.
//!
//! The worklist is sorted by typed-coverage gap so that landing
//! the patterns at the top of the list unblocks the most fixtures.
//!
//! This test ALWAYS passes; it's a reporting tool, not a regression
//! check. Run with `cargo test -p skotch-driver --test
//! typed_vs_legacy_worklist -- --nocapture` to see the summary.

use std::path::PathBuf;

use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::{MirFunction, MirModule, Terminator};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Discover all supported fixtures (any with `status = "supported"` in meta.toml).
fn discover_fixtures() -> Vec<String> {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let mut fixtures = Vec::new();
    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return fixtures;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("meta.toml");
        let input_path = entry.path().join("input.kt");
        if !input_path.exists() {
            continue;
        }
        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }
        fixtures.push(name);
    }
    fixtures.sort();
    fixtures
}

/// Compute a coverage metric for one MirModule.
#[derive(Default, Debug, Clone, Copy)]
struct Metrics {
    fn_count: usize,
    class_count: usize,
    block_count: usize,
    stmt_count: usize,
    /// Functions with empty/placeholder body (block 0 is just Return with no stmts).
    placeholder_fn_count: usize,
}

fn measure_function(f: &MirFunction, m: &mut Metrics) {
    m.fn_count += 1;
    m.block_count += f.blocks.len();
    let total_stmts: usize = f.blocks.iter().map(|b| b.stmts.len()).sum();
    m.stmt_count += total_stmts;
    let is_placeholder = f.blocks.len() == 1
        && f.blocks[0].stmts.is_empty()
        && matches!(f.blocks[0].terminator, Terminator::Return);
    if is_placeholder {
        m.placeholder_fn_count += 1;
    }
}

fn measure(module: &MirModule) -> Metrics {
    let mut m = Metrics::default();
    for f in &module.functions {
        measure_function(f, &mut m);
    }
    m.class_count = module.classes.len();
    for c in &module.classes {
        measure_function(&c.constructor, &mut m);
        for sc in &c.secondary_constructors {
            measure_function(sc, &mut m);
        }
        for f in &c.methods {
            measure_function(f, &mut m);
        }
    }
    m
}

#[test]
#[ignore = "reporting tool; run with --ignored to emit the worklist"]
fn emit_typed_vs_legacy_worklist() {
    // Large stack — the legacy mir-lower uses deep recursion on
    // some fixtures and overflows the default test stack.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run_emit_typed_vs_legacy_worklist)
        .expect("spawn worklist thread")
        .join()
        .expect("worklist thread");
}

fn run_emit_typed_vs_legacy_worklist() {
    let fixtures = discover_fixtures();
    eprintln!("Scanning {} supported fixtures...", fixtures.len());

    let mut rows: Vec<Row> = Vec::with_capacity(fixtures.len());

    for name in &fixtures {
        let input_path = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let Ok(source) = std::fs::read_to_string(&input_path) else {
            continue;
        };

        // Both pipelines need an Interner + Diagnostics, but we
        // discard the diagnostics for this report.
        let legacy_metrics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut interner = Interner::new();
            let mut diags = Diagnostics::new();
            let file_id = skotch_span::FileId(0);
            let module = skotch_driver::compile_source(
                &source,
                file_id,
                "InputKt",
                &mut interner,
                &mut diags,
                None,
            );
            measure(&module)
        }));

        let typed_metrics = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut interner = Interner::new();
            let mut diags = Diagnostics::new();
            let module = skotch_driver::typed::compile_source(
                &source,
                "input.kt",
                "InputKt",
                &mut interner,
                &mut diags,
                None,
            );
            measure(&module)
        }));

        let row = Row {
            name: name.clone(),
            legacy: legacy_metrics.unwrap_or_default(),
            typed: typed_metrics.unwrap_or_default(),
        };
        rows.push(row);
    }

    // Compute completeness ratio = typed_stmts / legacy_stmts.
    // Sort ascending so the worst-covered fixtures come first.
    rows.sort_by(|a, b| {
        let a_ratio = a.completeness_ratio();
        let b_ratio = b.completeness_ratio();
        a_ratio
            .partial_cmp(&b_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total = rows.len();
    let fully_covered = rows
        .iter()
        .filter(|r| (r.completeness_ratio() - 1.0).abs() < 0.001)
        .count();
    let empty_typed = rows.iter().filter(|r| r.typed.stmt_count == 0).count();

    eprintln!("\n=== Typed-vs-Legacy MIR coverage worklist ===");
    eprintln!(
        "Total fixtures: {}\nFully covered: {} ({}%)\nTyped empty: {} ({}%)",
        total,
        fully_covered,
        if total > 0 {
            100 * fully_covered / total
        } else {
            0
        },
        empty_typed,
        if total > 0 {
            100 * empty_typed / total
        } else {
            0
        }
    );
    eprintln!("(coverage = typed_stmts / legacy_stmts; lower = worse)");

    // Write the full worklist to disk so it can be inspected /
    // diffed across sessions.
    let mut out = String::new();
    out.push_str(&format!(
        "# Typed-vs-Legacy MIR coverage worklist\n\
         # Generated by `cargo test -p skotch-driver --test typed_vs_legacy_worklist -- --ignored --nocapture`\n\
         # Total: {total} | Fully covered: {fully_covered} | Typed empty: {empty_typed}\n\n"
    ));
    out.push_str("# coverage\tfixture\tlegacy_stmts\ttyped_stmts\tlegacy_fns\ttyped_fns\ttyped_placeholders\n");
    for r in &rows {
        out.push_str(&format!(
            "{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.completeness_ratio(),
            r.name,
            r.legacy.stmt_count,
            r.typed.stmt_count,
            r.legacy.fn_count,
            r.typed.fn_count,
            r.typed.placeholder_fn_count,
        ));
    }
    let worklist_path = workspace_root().join("tests/fixtures/typed_worklist.txt");
    std::fs::write(&worklist_path, &out).expect("write typed_worklist.txt");
    eprintln!("\nWrote {} bytes to {}", out.len(), worklist_path.display());

    // Top-20 quick summary for the console.
    eprintln!("\nTop-20 worst-covered fixtures (these unblock the most):");
    for r in rows.iter().take(20) {
        eprintln!(
            "  {:.3}  {}  ({} → {} stmts; {} placeholders)",
            r.completeness_ratio(),
            r.name,
            r.legacy.stmt_count,
            r.typed.stmt_count,
            r.typed.placeholder_fn_count,
        );
    }

    // Note: this test always succeeds; failure means we couldn't
    // discover or compile any fixtures, which is a real bug.
    assert!(!rows.is_empty(), "no fixtures discovered");
}

struct Row {
    name: String,
    legacy: Metrics,
    typed: Metrics,
}

impl Row {
    fn completeness_ratio(&self) -> f64 {
        if self.legacy.stmt_count == 0 {
            // No useful comparison; treat as fully covered.
            return 1.0;
        }
        self.typed.stmt_count as f64 / self.legacy.stmt_count as f64
    }
}
