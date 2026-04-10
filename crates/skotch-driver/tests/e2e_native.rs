//! Behavioral end-to-end test for the native target.
//!
//! For each supported fixture, run `skotch emit --target native`
//! (which internally goes MIR → klib → LLVM IR → clang → executable),
//! invoke the produced binary, and assert its stdout matches the
//! committed `expected/native/<f>/run.stdout`.
//!
//! Gated on `clang` being on `PATH`. Skipped silently otherwise.

use std::path::PathBuf;
use std::process::Command;

use skotch_driver::{emit, EmitOptions, Target};

const SUPPORTED: &[&str] = &[
    "01-fun-main-empty",
    "02-println-string-literal",
    "03-println-int-literal",
    "04-val-string",
    "05-string-template-simple",
    "06-arithmetic-int",
    "07-if-expression",
    "08-function-call",
    "09-multiple-statements",
    "10-top-level-val",
    "38-string-template-expr",
    "39-raw-string",
];

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn skotch_native_binaries_match_committed_stdout() {
    if which::which("clang").is_err() {
        eprintln!("[skip] clang not on PATH");
        return;
    }

    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let stdout_file = workspace_root()
            .join("tests/fixtures/expected/native")
            .join(name)
            .join("run.stdout");
        if !stdout_file.exists() {
            continue;
        }
        let tmp =
            std::env::temp_dir().join(format!("skotch-e2e-native-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bin = tmp.join("hello");
        emit(&EmitOptions {
            input: input.clone(),
            output: bin.clone(),
            target: Target::Native,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let out = Command::new(&bin).output().expect("running native binary");
        // Strip CR from both sides — on Windows, libc's stdout
        // translates `\n` to `\r\n` in text mode, while the
        // committed `run.stdout` is pinned to LF by `.gitattributes`.
        let expected = std::fs::read_to_string(&stdout_file)
            .unwrap()
            .replace('\r', "");
        let actual = String::from_utf8_lossy(&out.stdout).replace('\r', "");
        if !out.status.success() {
            failures.push(format!(
                "{name}: native binary exited {}: stderr={}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            continue;
        }
        if actual != expected {
            failures.push(format!(
                "{name}: stdout mismatch\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed e2e native run:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skotch_native_matches_kotlinc_native_observable_behavior() {
    // For every fixture where we have *both* run.stdout and
    // kotlinc-native.run.stdout, they should match exactly. This is
    // the strongest possible cross-compiler validation: same source,
    // same observable behavior.
    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let dir = workspace_root()
            .join("tests/fixtures/expected/native")
            .join(name);
        let skotch = dir.join("run.stdout");
        let kn = dir.join("kotlinc-native.run.stdout");
        if !skotch.exists() || !kn.exists() {
            continue;
        }
        let skotch_text = std::fs::read_to_string(&skotch).unwrap().replace('\r', "");
        let kn_text = std::fs::read_to_string(&kn).unwrap().replace('\r', "");
        if skotch_text != kn_text {
            failures.push(format!(
                "{name}: skotch vs kotlinc-native stdout differ\n  skotch: {skotch_text:?}\n  kn:   {kn_text:?}"
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) have skotch/kotlinc-native stdout drift:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
