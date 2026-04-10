//! Behavioral end-to-end test for the DEX backend.
//!
//! For each supported fixture, run `skotch emit --target dex` and then
//! shell out to Android's `dexdump` to verify the file is structurally
//! valid. Gated on `dexdump` being available — contributors without
//! the Android SDK build-tools installed see a `[skip]` line and the
//! test passes.
//!
//! Locating `dexdump`: we look at the standard
//! `~/Library/Android/sdk/build-tools/<version>/dexdump` path used on
//! macOS first, then fall back to `which dexdump`.

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

fn locate_dexdump() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let sdk = PathBuf::from(home).join("Library/Android/sdk/build-tools");
        if let Ok(dir) = std::fs::read_dir(&sdk) {
            let mut versions: Vec<PathBuf> = dir
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("dexdump").exists())
                .collect();
            versions.sort();
            if let Some(latest) = versions.last() {
                return Some(latest.join("dexdump"));
            }
        }
    }
    which::which("dexdump").ok()
}

#[test]
fn skotch_dex_files_pass_dexdump() {
    let dexdump = match locate_dexdump() {
        Some(p) => p,
        None => {
            eprintln!("[skip] dexdump not on PATH or in Android SDK");
            return;
        }
    };

    let mut failures: Vec<String> = Vec::new();
    for &name in SUPPORTED {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let tmp =
            std::env::temp_dir().join(format!("skotch-e2e-dex-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dex_path = tmp.join("classes.dex");
        emit(&EmitOptions {
            input: input.clone(),
            output: dex_path.clone(),
            target: Target::Dex,
            norm_out: None,
        })
        .unwrap_or_else(|e| panic!("emit failed for {name}: {e}"));

        let out = Command::new(&dexdump)
            .arg(&dex_path)
            .output()
            .expect("running dexdump");
        let stderr = String::from_utf8_lossy(&out.stderr);
        // dexdump returns 0 even on parse errors but writes "Failure to
        // verify" to stderr. Fail the test if either signal is present.
        if !out.status.success() || stderr.contains("Failure") || stderr.contains("Error") {
            failures.push(format!(
                "{name}: dexdump failed (status={}, stderr={})",
                out.status,
                stderr.trim()
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed dexdump verification:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
