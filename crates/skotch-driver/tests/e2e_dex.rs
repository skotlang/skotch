//! Behavioral end-to-end test for the DEX backend.
//!
//! POLICY: Every fixture that passes e2e on JVM must also compile to
//! valid DEX. This test dynamically discovers all fixtures marked
//! "supported" with a `run.stdout` file and attempts DEX compilation.
//! It then validates the output with `dexdump`.
//!
//! Fixtures that fail DEX compilation are reported but the test tracks
//! them separately so we can incrementally fix the backend.
//!
//! Gated on `dexdump` being available via Android SDK.

use std::path::PathBuf;
use std::process::Command;

use skotch_driver::{emit, EmitOptions, Target};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

fn locate_dexdump() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        let Some(home) = std::env::var_os(var) else {
            continue;
        };
        let build_tools = PathBuf::from(home).join("build-tools");
        let Ok(dir) = std::fs::read_dir(&build_tools) else {
            continue;
        };
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
    which::which("dexdump").ok()
}

/// Discover all fixtures eligible for e2e testing (supported + run.stdout).
fn discover_e2e_fixtures() -> Vec<String> {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let mut fixtures = Vec::new();
    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return fixtures;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("meta.toml");
        let stdout_path = jvm_dir.join(&name).join("run.stdout");

        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }

        if !stdout_path.exists() {
            continue;
        }

        fixtures.push(name);
    }

    fixtures.sort();
    fixtures
}

#[test]
fn skotch_dex_compilation_and_dexdump_validation() {
    let dexdump = match locate_dexdump() {
        Some(p) => p,
        None => {
            eprintln!("[skip] dexdump not on PATH or in Android SDK");
            return;
        }
    };

    let fixtures = discover_e2e_fixtures();
    if fixtures.is_empty() {
        eprintln!("[skip] no eligible fixtures found");
        return;
    }

    let mut compile_failures: Vec<String> = Vec::new();
    let mut dexdump_failures: Vec<String> = Vec::new();
    let mut passed = 0;

    for name in &fixtures {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");

        let tmp =
            std::env::temp_dir().join(format!("skotch-e2e-dex-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dex_path = tmp.join("classes.dex");

        // Attempt DEX compilation. Panics in the backend are caught as
        // compile errors (the emit function returns Err).
        let emit_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit(&EmitOptions {
                input: input.clone(),
                output: dex_path.clone(),
                target: Target::Dex,
                norm_out: None,
            })
        }));

        match emit_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                compile_failures.push(format!("{name}: compile error: {e}"));
                let _ = std::fs::remove_dir_all(&tmp);
                continue;
            }
            Err(_) => {
                compile_failures.push(format!("{name}: DEX backend panicked"));
                let _ = std::fs::remove_dir_all(&tmp);
                continue;
            }
        }

        // Validate with dexdump.
        let out = Command::new(&dexdump)
            .arg(&dex_path)
            .output()
            .expect("running dexdump");

        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() || stderr.contains("Failure") || stderr.contains("Error") {
            dexdump_failures.push(format!(
                "{name}: dexdump failed: {}",
                stderr.lines().next().unwrap_or("(no details)")
            ));
        } else {
            passed += 1;
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    eprintln!(
        "e2e_dex: {passed} passed, {} compile failures, {} dexdump failures (of {} eligible)",
        compile_failures.len(),
        dexdump_failures.len(),
        fixtures.len()
    );

    // Report all failures but don't panic on compile failures (known DEX gaps).
    // DO panic on dexdump failures — if we produce a .dex it must be valid.
    if !compile_failures.is_empty() {
        eprintln!(
            "DEX compile failures (known gaps, {} total):\n  - {}",
            compile_failures.len(),
            compile_failures.join("\n  - ")
        );
    }

    if !dexdump_failures.is_empty() {
        panic!(
            "{} fixture(s) produced invalid DEX:\n  - {}",
            dexdump_failures.len(),
            dexdump_failures.join("\n  - ")
        );
    }
}
