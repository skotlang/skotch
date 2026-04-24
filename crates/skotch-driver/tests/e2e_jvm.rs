//! Behavioral end-to-end test: build each supported fixture with skotch,
//! run the resulting `.class` through `java`, and assert stdout matches
//! the committed `run.stdout`.
//!
//! Unlike the old hardcoded list of 12 fixtures, this test **dynamically
//! discovers** every fixture that has:
//!   1. `status = "supported"` in its `meta.toml`
//!   2. A `run.stdout` file in `tests/fixtures/expected/jvm/<name>/`
//!
//! This ensures that marking a fixture as "supported" actually means it
//! produces correct output — not just that the compiler doesn't crash.
//!
//! Gated on `java` being available on `PATH`.

use std::path::PathBuf;
use std::process::Command;

use skotch_driver::{emit, EmitOptions, Target};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Discover all fixtures that are marked "supported" in meta.toml and
/// have a committed `run.stdout` expected output file.
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

        // Must have meta.toml with status = "supported"
        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("status") || !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }

        // Must have a run.stdout expected output
        if !stdout_path.exists() {
            continue;
        }

        fixtures.push(name);
    }

    fixtures.sort();
    fixtures
}

#[test]
fn skotch_classes_run_under_java_and_stdout_matches() {
    let java = match which::which("java") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[skip] java not on PATH");
            return;
        }
    };

    // Locate kotlin-stdlib.jar so classes that reference
    // kotlin.collections.CollectionsKt (e.g. listOf) can resolve at runtime.
    let kotlin_stdlib: Option<std::path::PathBuf> = match skotch_classinfo::find_kotlin_lib_dir() {
        Ok(d) => {
            let jar = d.join("kotlin-stdlib.jar");
            if jar.exists() {
                eprintln!("  kotlin-stdlib: {}", jar.display());
                Some(jar)
            } else {
                eprintln!(
                    "[warn] kotlin-stdlib.jar not found at {} — \
                     lambda/collection fixtures will fail",
                    jar.display()
                );
                None
            }
        }
        Err(e) => {
            eprintln!(
                "[warn] could not locate Kotlin stdlib: {e} — \
                 lambda/collection fixtures will fail.\n  \
                 hint: set KOTLIN_HOME or add kotlin-stdlib.jar to CLASSPATH"
            );
            None
        }
    };

    let fixtures = discover_e2e_fixtures();
    if fixtures.is_empty() {
        eprintln!("[skip] no eligible fixtures found");
        return;
    }

    let mut failures: Vec<String> = Vec::new();
    let mut passed = 0;
    let skipped = 0;

    for name in &fixtures {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let stdout_file = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name)
            .join("run.stdout");

        let tmp = std::env::temp_dir().join(format!("skotch-e2e-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let class_path = tmp.join("InputKt.class");

        // Try to compile. If compilation fails, record failure.
        let emit_result = emit(&EmitOptions {
            input: input.clone(),
            output: class_path.clone(),
            target: Target::Jvm,
            norm_out: None,
        });

        if let Err(e) = emit_result {
            failures.push(format!("{name}: compilation failed: {e}"));
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }

        // Build classpath: temp dir + kotlin-stdlib.jar + kotlinx-coroutines-core-jvm.jar.
        let sep = if cfg!(windows) { ";" } else { ":" };
        let mut cp_str = tmp.display().to_string();
        if let Some(ref stdlib) = kotlin_stdlib {
            cp_str = format!("{cp_str}{sep}{}", stdlib.display());
            // Include kotlinx-coroutines-core-jvm.jar for
            // coroutine fixtures (runBlocking, delay).
            let coroutines = stdlib.with_file_name("kotlinx-coroutines-core-jvm.jar");
            if coroutines.exists() {
                cp_str = format!("{cp_str}{sep}{}", coroutines.display());
            }
        }

        // Run under java.
        let out = Command::new(&java)
            .arg("-cp")
            .arg(&cp_str)
            .arg("InputKt")
            .output()
            .expect("running java");

        let expected = std::fs::read_to_string(&stdout_file)
            .unwrap()
            .replace('\r', "");
        let actual = String::from_utf8_lossy(&out.stdout).replace('\r', "");

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // VerifyError or other JVM errors mean our bytecode is wrong
            failures.push(format!(
                "{name}: java exited {}: {}",
                out.status,
                stderr.lines().next().unwrap_or("(no stderr)")
            ));
        } else if actual != expected {
            failures.push(format!(
                "{name}: stdout mismatch\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        } else {
            passed += 1;
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    eprintln!(
        "e2e_jvm: {passed} passed, {} failed, {skipped} skipped (of {} eligible)",
        failures.len(),
        fixtures.len()
    );

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed e2e:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
