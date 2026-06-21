// Quick probe: dump legacy vs typed MIR for a single named fixture.
// Run with: FIXTURE=270-string-when-match cargo test -p skotch-driver --test probe_one_fixture -- --ignored --nocapture
use skotch_diagnostics::Diagnostics;
use skotch_intern::Interner;
use skotch_mir::{MirModule, Stmt as MStmt, Terminator};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().parent().unwrap().to_path_buf()
}

fn dump(label: &str, m: &MirModule) {
    eprintln!("--- {label} ---");
    for f in &m.functions {
        eprintln!(
            "fn {} (params={}, blocks={})",
            f.name,
            f.params.len(),
            f.blocks.len()
        );
        for (i, blk) in f.blocks.iter().enumerate() {
            eprintln!(
                "  blk{}: {} stmts, term={:?}",
                i,
                blk.stmts.len(),
                summary_term(&blk.terminator)
            );
            for s in &blk.stmts {
                eprintln!("    {}", short(s));
            }
        }
    }
}

fn summary_term(t: &Terminator) -> String {
    match t {
        Terminator::Return => "Ret".to_string(),
        Terminator::ReturnValue(s) => format!("RetV({})", s.0),
        Terminator::Goto(b) => format!("Goto({b})"),
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => {
            format!("Br({} -> {}/{})", cond.0, then_block, else_block)
        }
        _ => format!("{t:?}"),
    }
}

fn short(s: &MStmt) -> String {
    let full = format!("{s:?}");
    if full.len() > 140 {
        format!("{}...", &full[..137])
    } else {
        full
    }
}

#[test]
#[ignore]
fn probe_one() {
    let name = std::env::var("FIXTURE").expect("set FIXTURE=name");
    let input_path = workspace_root()
        .join("tests/fixtures/inputs")
        .join(&name)
        .join("input.kt");
    let source = std::fs::read_to_string(&input_path).expect("read input.kt");
    eprintln!("=== Fixture: {name} ===");
    eprintln!("Source:\n{source}\n");

    let legacy = {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        let file_id = skotch_span::FileId(0);
        skotch_driver::compile_source(&source, file_id, "InputKt", &mut interner, &mut diags, None)
    };
    let typed = {
        let mut interner = Interner::new();
        let mut diags = Diagnostics::new();
        skotch_driver::typed::compile_source(
            &source,
            "input.kt",
            "InputKt",
            &mut interner,
            &mut diags,
            None,
        )
    };
    dump("LEGACY", &legacy);
    dump("TYPED", &typed);
}
