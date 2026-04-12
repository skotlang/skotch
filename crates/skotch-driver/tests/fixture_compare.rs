//! JVM-target golden comparison tests.
//!
//! Dynamically discovers fixtures with committed JVM goldens and verifies:
//! 1. skotch .class output is byte-equal to committed golden
//! 2. Normalized text matches committed skotch.norm.txt

use std::path::PathBuf;

use skotch_driver::{emit, EmitOptions, Target};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Discover fixtures with committed JVM goldens AND supported status.
fn discover_jvm_golden_fixtures() -> Vec<String> {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let jvm_dir = workspace_root().join("tests/fixtures/expected/jvm");

    let mut fixtures = Vec::new();
    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return fixtures;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("meta.toml");
        let golden = jvm_dir.join(&name).join("skotch.class");

        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }

        if !golden.exists() {
            continue;
        }

        fixtures.push(name);
    }

    fixtures.sort();
    fixtures
}

#[test]
fn skotch_self_consistent_with_committed_goldens() {
    let fixtures = discover_jvm_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let golden = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name)
            .join("skotch.class");

        let tmp =
            std::env::temp_dir().join(format!("skotch-jvm-cmp-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("InputKt.class");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit(&EmitOptions {
                input: input.clone(),
                output: out.clone(),
                target: Target::Jvm,
                norm_out: None,
            })
        }));

        match result {
            Ok(Ok(())) => {
                let new_bytes = std::fs::read(&out).unwrap();
                let golden_bytes = std::fs::read(&golden).unwrap();
                if new_bytes != golden_bytes {
                    failures.push(format!(
                        "{name}: skotch.class drift ({} vs {} bytes)",
                        new_bytes.len(),
                        golden_bytes.len()
                    ));
                }
            }
            Ok(Err(e)) => {
                failures.push(format!("{name}: compile error: {e}"));
            }
            Err(_) => {
                failures.push(format!("{name}: JVM backend panicked"));
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skotch.class goldens:\n  - {}\n\nRefresh with: cargo xtask gen-fixtures --target jvm",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skotch_norm_matches_committed_skotch_norm() {
    let fixtures = discover_jvm_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let jvm_dir = workspace_root()
            .join("tests/fixtures/expected/jvm")
            .join(name);
        let golden_class = jvm_dir.join("skotch.class");
        let golden_norm = jvm_dir.join("skotch.norm.txt");
        if !golden_norm.exists() {
            continue;
        }
        let bytes = std::fs::read(&golden_class).unwrap();
        let Ok(normalized) = skotch_classfile_norm::normalize_default(&bytes) else {
            failures.push(format!("{name}: normalize failed"));
            continue;
        };
        let golden_text = std::fs::read_to_string(&golden_norm)
            .unwrap()
            .replace('\r', "");
        let norm_text = normalized.as_text().replace('\r', "");
        if norm_text != golden_text {
            failures.push(format!("{name}: normalizer output drifted"));
        }
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) have skotch.norm.txt drift:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}
