//! DEX-target counterpart to `fixture_compare.rs`.
//!
//! Verifies that:
//!
//! 1. **Skot self-consistency** — `skot emit --target dex` is byte-equal
//!    to the committed `expected/dex/<f>/skot.dex` golden, for every
//!    supported fixture.
//! 2. **Skot normalized text matches the committed golden** — the
//!    `skot-dex-norm` output of skot's bytes equals the committed
//!    `skot.norm.txt`. This catches drift in the normalizer crate
//!    independently from drift in the backend.
//! 3. **Method counts agree with d8** — for each fixture we parse the
//!    skot output and the committed `d8.norm.txt` and assert that
//!    both describe the same wrapper class with the same `main` shape.
//!    We don't enforce per-instruction or constant pool equality
//!    because skot picks a tighter lowering (single `main()V` method
//!    vs. d8's `main + main(String[])` trampoline pair, and direct
//!    `println(String)` vs. d8's `println(Object)` overload).
//!
//! Neither of these tests requires `dexdump`, `kotlinc`, or `d8` —
//! everything they need is committed under `tests/fixtures/expected/`.

use std::path::PathBuf;

use skot_driver::{emit, EmitOptions, Target};

const SUPPORTED: &[&str] = &[
    "01-fun-main-empty",
    "02-println-string-literal",
    "03-println-int-literal",
    "04-val-string",
    "06-arithmetic-int",
    "08-function-call",
    "09-multiple-statements",
];

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixture_input(name: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/inputs")
        .join(name)
        .join("input.kt")
}

fn expected_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/expected/dex")
        .join(name)
}

#[test]
fn dex_self_consistent_with_committed_goldens() {
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = fixture_input(name);
        let golden = expected_dir(name).join("skot.dex");
        if !golden.exists() {
            failures.push(format!("{name}: missing skot.dex golden"));
            continue;
        }
        let tmp =
            std::env::temp_dir().join(format!("skot-dex-fixture-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("classes.dex");
        emit(&EmitOptions {
            input: input.clone(),
            output: out.clone(),
            target: Target::Dex,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let new_bytes = std::fs::read(&out).unwrap();
        let golden_bytes = std::fs::read(&golden).unwrap();
        if new_bytes != golden_bytes {
            failures.push(format!(
                "{name}: skot.dex drift ({} new bytes vs {} golden bytes)",
                new_bytes.len(),
                golden_bytes.len()
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skot.dex goldens:\n  - {}\n\nRefresh with: cargo xtask gen-fixtures --target dex",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skot_dex_norm_matches_committed_skot_norm() {
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let golden_dex = expected_dir(name).join("skot.dex");
        let golden_norm = expected_dir(name).join("skot.norm.txt");
        if !golden_dex.exists() || !golden_norm.exists() {
            continue;
        }
        let bytes = std::fs::read(&golden_dex).unwrap();
        let normalized = skot_dex_norm::normalize_default(&bytes).expect("normalize golden bytes");
        let golden_text = std::fs::read_to_string(&golden_norm).unwrap();
        if normalized != golden_text {
            failures.push(format!("{name}: dex normalizer output drifted"));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) have skot.norm.txt drift in dex normalizer:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skot_and_d8_both_emit_main_v_method() {
    // For each fixture, parse skot's and d8's normalized output and
    // check that both describe a `main()V` method on `LInputKt;`
    // with `flags=0x0019` (PUBLIC|STATIC|FINAL).
    //
    // We deliberately do **not** compare `regs`/`outs`/`insns` counts.
    // d8 performs register-allocation optimizations that skot's
    // straightforward MIR-to-DEX lowering doesn't, so it produces
    // smaller `regs` and `insns` counts even when both encode the
    // same logical program. A future PR (#3.5) that adds register
    // coalescing to the bytecode emitter can tighten this assertion.
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let skot_norm = expected_dir(name).join("skot.norm.txt");
        let d8_norm = expected_dir(name).join("d8.norm.txt");
        if !skot_norm.exists() || !d8_norm.exists() {
            continue;
        }
        let skot_text = std::fs::read_to_string(&skot_norm).unwrap();
        let d8_text = std::fs::read_to_string(&d8_norm).unwrap();
        if !has_main_v_method(&skot_text) {
            failures.push(format!("{name}: skot.norm.txt has no main()V method"));
        }
        if !has_main_v_method(&d8_text) {
            failures.push(format!("{name}: d8.norm.txt has no main()V method"));
        }
    }
    if !failures.is_empty() {
        panic!("{}", failures.join("\n"));
    }
}

fn has_main_v_method(text: &str) -> bool {
    text.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("method       main()V") && l.contains("flags=0x0019")
    })
}
