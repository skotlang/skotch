//! Native-target counterpart to `fixture_compare.rs` and
//! `fixture_compare_dex.rs`. Covers all three stages of the
//! Kotlin-Native-style multi-stage pipeline:
//!
//! - **klib stage** — `expected/klib/<f>/skot.klib` byte-equality
//!   plus `klib_round_trip` (write → read → re-write produces the
//!   same bytes).
//! - **LLVM IR stage** — `expected/llvm/<f>/skot.ll` byte-equality
//!   plus normalized text equality.
//! - **kotlinc-native cross-check** — verifies that the committed
//!   `kotlinc-native.summary.txt` reports a `main` function for each
//!   fixture, so we know the reference toolchain agrees the input
//!   produces an entry point.
//!
//! Behavioral validation (running the binary and checking stdout) is
//! split into `e2e_native.rs` so it can be skipped on machines
//! without `clang`.

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

fn klib_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/expected/klib")
        .join(name)
}

fn llvm_dir(name: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/expected/llvm")
        .join(name)
}

#[test]
fn klib_self_consistent_with_committed_goldens() {
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = fixture_input(name);
        let golden = klib_dir(name).join("skot.klib");
        if !golden.exists() {
            failures.push(format!("{name}: missing skot.klib golden"));
            continue;
        }
        let tmp =
            std::env::temp_dir().join(format!("skot-klib-fixture-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("hello.klib");
        emit(&EmitOptions {
            input: input.clone(),
            output: out.clone(),
            target: Target::Klib,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let new_bytes = std::fs::read(&out).unwrap();
        let golden_bytes = std::fs::read(&golden).unwrap();
        if new_bytes != golden_bytes {
            failures.push(format!(
                "{name}: skot.klib drift ({} new bytes vs {} golden bytes)",
                new_bytes.len(),
                golden_bytes.len()
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skot.klib goldens:\n  - {}\n\nRefresh with: cargo xtask gen-fixtures --target klib",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn klib_round_trip_preserves_module() {
    // Direct round-trip: build MIR, write klib, read it back, compare
    // wrapper class + string pool. We don't compare the structs
    // directly because Rvalue/Stmt aren't PartialEq. The functional
    // shape is what matters.
    for &name in SUPPORTED {
        let golden = klib_dir(name).join("skot.klib");
        if !golden.exists() {
            continue;
        }
        let bytes = std::fs::read(&golden).unwrap();
        let (m, manifest) = skot_backend_klib::read_klib(&bytes)
            .unwrap_or_else(|e| panic!("read_klib failed for {name}: {e}"));
        assert_eq!(manifest.compiler, "skot", "{name}");
        assert_eq!(m.wrapper_class, "InputKt", "{name}");
        // Re-emit and verify byte-equality. This catches any
        // non-determinism in the writer.
        let bytes2 = skot_backend_klib::write_klib(&m, &manifest.native_targets).unwrap();
        assert_eq!(
            bytes, bytes2,
            "{name}: klib round-trip is non-deterministic"
        );
    }
}

#[test]
fn llvm_self_consistent_with_committed_goldens() {
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = fixture_input(name);
        let golden = llvm_dir(name).join("skot.ll");
        if !golden.exists() {
            failures.push(format!("{name}: missing skot.ll golden"));
            continue;
        }
        let tmp =
            std::env::temp_dir().join(format!("skot-llvm-fixture-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("hello.ll");
        emit(&EmitOptions {
            input: input.clone(),
            output: out.clone(),
            target: Target::Llvm,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let new = std::fs::read_to_string(&out).unwrap();
        let golden_text = std::fs::read_to_string(&golden).unwrap();
        if new != golden_text {
            failures.push(format!("{name}: skot.ll drift"));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skot.ll goldens:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skot_llvm_norm_matches_committed_skot_norm() {
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let golden_ll = llvm_dir(name).join("skot.ll");
        let golden_norm = llvm_dir(name).join("skot.norm.txt");
        if !golden_ll.exists() || !golden_norm.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&golden_ll).unwrap();
        let normalized = skot_llvm_norm::normalize(&text);
        let golden_norm_text = std::fs::read_to_string(&golden_norm).unwrap();
        if normalized != golden_norm_text {
            failures.push(format!("{name}: llvm normalizer output drifted"));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) have skot.norm.txt drift in llvm normalizer:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn kotlinc_native_summary_reports_main_for_every_fixture() {
    // For each fixture we expect kotlinc-native to have produced an
    // LLVM IR dump containing a `main` function (Kotlin's
    // `kfun:#main` or the C entry `Konan_main`). The summary file
    // committed by xtask records the relevant `define` lines.
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let summary_path = llvm_dir(name).join("kotlinc-native.summary.txt");
        if !summary_path.exists() {
            // Reference unavailable on the machine that committed
            // the goldens; skip silently.
            continue;
        }
        let text = std::fs::read_to_string(&summary_path).unwrap();
        if !text.contains("main") {
            failures.push(format!("{name}: kotlinc-native summary mentions no main"));
        }
        // Sanity: kotlinc-native always emits >= 100 definitions
        // (just from its runtime). If we see 0, the summary is bogus.
        if !text.lines().any(|l| {
            l.starts_with("definitions:")
                && l.split_whitespace()
                    .last()
                    .and_then(|n| n.parse::<u32>().ok())
                    .map(|n| n > 100)
                    .unwrap_or(false)
        }) {
            failures.push(format!(
                "{name}: kotlinc-native summary has implausibly few definitions"
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed kotlinc-native summary checks:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn llvm_emission_routes_through_klib_pipeline() {
    // Functional check: the `Llvm` target's emit() path is
    // documented to round-trip through klib internally. Verify by
    // monkey-patching: if we manually do MIR → klib → llvm and
    // compare against `Target::Llvm`'s output, they should match
    // byte-for-byte.
    for &name in SUPPORTED {
        let input = fixture_input(name);
        let tmp = std::env::temp_dir().join(format!("skot-pipeline-{name}"));
        std::fs::create_dir_all(&tmp).unwrap();

        // Path A: skot emit --target llvm
        let a_path = tmp.join("a.ll");
        emit(&EmitOptions {
            input: input.clone(),
            output: a_path.clone(),
            target: Target::Llvm,
            norm_out: None,
        })
        .unwrap();
        let a = std::fs::read_to_string(&a_path).unwrap();

        // Path B: read the committed skot.klib + run compile_klib
        // directly. If the driver is honoring the multi-stage
        // pipeline both should match.
        let golden_klib = klib_dir(name).join("skot.klib");
        if !golden_klib.exists() {
            continue;
        }
        let klib_bytes = std::fs::read(&golden_klib).unwrap();
        let b = skot_backend_llvm::compile_klib(&klib_bytes).unwrap();
        assert_eq!(
            a, b,
            "{name}: --target llvm output does not match compile_klib() of the committed klib"
        );
    }
}
