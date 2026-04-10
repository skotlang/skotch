//! Behavioral end-to-end test: build each supported fixture with skot,
//! run the resulting `.class` through `java`, and assert stdout
//! matches the committed `run.stdout`.
//!
//! These tests are gated on `java` being available on `PATH`. They
//! print a clear `[skip]` line and pass when it's not — so contributors
//! without a JDK can still run the unit tests + the byte-level
//! comparisons in `fixture_compare.rs`.

use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn skot_classes_run_under_java_and_stdout_matches() {
    let java = match which::which("java") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[skip] java not on PATH");
            return;
        }
    };

    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let stdout_file = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name)
            .join("run.stdout");
        if !stdout_file.exists() {
            // The fixture doesn't have a captured reference stdout
            // (e.g. fixture 08 if kotlin-stdlib was missing at gen
            // time). Skip silently rather than fail.
            continue;
        }
        let tmp = std::env::temp_dir().join(format!("skot-e2e-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let class_path = tmp.join("InputKt.class");
        emit(&EmitOptions {
            input: input.clone(),
            output: class_path.clone(),
            target: Target::Jvm,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let out = Command::new(&java)
            .arg("-cp")
            .arg(&tmp)
            .arg("InputKt")
            .output()
            .expect("running java");
        let expected = std::fs::read_to_string(&stdout_file).unwrap();
        let actual = String::from_utf8_lossy(&out.stdout).to_string();
        if !out.status.success() {
            failures.push(format!(
                "{name}: java exited {}: stderr={}",
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
            "{} fixture(s) failed e2e:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
