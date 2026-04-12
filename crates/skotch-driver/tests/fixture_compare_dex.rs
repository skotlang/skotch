//! DEX-target golden comparison tests.
//!
//! Dynamically discovers fixtures with committed DEX goldens and verifies:
//! 1. skotch DEX output is byte-equal to committed golden
//! 2. Normalized text matches committed skotch.norm.txt
//! 3. Both skotch and d8 describe the same main()V method shape

use std::path::PathBuf;

use skotch_driver::{emit, EmitOptions, Target};

fn workspace_root() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent().unwrap().parent().unwrap().to_path_buf()
}

/// Discover fixtures that have committed DEX goldens AND are marked supported.
fn discover_dex_golden_fixtures() -> Vec<String> {
    let inputs_dir = workspace_root().join("tests/fixtures/inputs");
    let dex_dir = workspace_root().join("tests/fixtures/expected/dex");

    let mut fixtures = Vec::new();
    let Ok(entries) = std::fs::read_dir(&inputs_dir) else {
        return fixtures;
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = entry.path().join("meta.toml");
        let golden_dex = dex_dir.join(&name).join("skotch.dex");

        // Must be supported
        if let Ok(meta) = std::fs::read_to_string(&meta_path) {
            if !meta.contains("\"supported\"") {
                continue;
            }
        } else {
            continue;
        }

        // Must have committed DEX golden
        if !golden_dex.exists() {
            continue;
        }

        fixtures.push(name);
    }

    fixtures.sort();
    fixtures
}

#[test]
fn dex_self_consistent_with_committed_goldens() {
    let fixtures = discover_dex_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let input = workspace_root()
            .join("tests/fixtures/inputs")
            .join(name)
            .join("input.kt");
        let golden = workspace_root()
            .join("tests/fixtures/expected/dex")
            .join(name)
            .join("skotch.dex");

        let tmp =
            std::env::temp_dir().join(format!("skotch-dex-cmp-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let out = tmp.join("classes.dex");

        // Catch panics from the DEX backend (register overflow etc.)
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            emit(&EmitOptions {
                input: input.clone(),
                output: out.clone(),
                target: Target::Dex,
                norm_out: None,
            })
        }));

        match result {
            Ok(Ok(())) => {
                let new_bytes = std::fs::read(&out).unwrap();
                let golden_bytes = std::fs::read(&golden).unwrap();
                if new_bytes != golden_bytes {
                    failures.push(format!(
                        "{name}: skotch.dex drift ({} vs {} bytes)",
                        new_bytes.len(),
                        golden_bytes.len()
                    ));
                }
            }
            Ok(Err(e)) => {
                failures.push(format!("{name}: compile error: {e}"));
            }
            Err(_) => {
                failures.push(format!("{name}: DEX backend panicked"));
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    if !failures.is_empty() {
        panic!(
            "{} fixture(s) drifted from committed skotch.dex goldens:\n  - {}\n\nRefresh with: cargo xtask gen-fixtures --target dex",
            failures.len(),
            failures.join("\n  - ")
        );
    }
}

#[test]
fn skotch_dex_norm_matches_committed_skotch_norm() {
    let fixtures = discover_dex_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let dex_dir = workspace_root()
            .join("tests/fixtures/expected/dex")
            .join(name);
        let golden_dex = dex_dir.join("skotch.dex");
        let golden_norm = dex_dir.join("skotch.norm.txt");
        if !golden_norm.exists() {
            continue;
        }
        let bytes = std::fs::read(&golden_dex).unwrap();
        let Ok(normalized) = skotch_dex_norm::normalize_default(&bytes) else {
            failures.push(format!("{name}: normalize failed"));
            continue;
        };
        let golden_text = std::fs::read_to_string(&golden_norm)
            .unwrap()
            .replace('\r', "");
        let normalized = normalized.replace('\r', "");
        if normalized != golden_text {
            failures.push(format!("{name}: dex normalizer output drifted"));
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

#[test]
fn skotch_and_d8_both_emit_main_v_method() {
    let fixtures = discover_dex_golden_fixtures();
    let mut failures: Vec<String> = Vec::new();

    for name in &fixtures {
        let dex_dir = workspace_root()
            .join("tests/fixtures/expected/dex")
            .join(name);
        let skotch_norm = dex_dir.join("skotch.norm.txt");
        let d8_norm = dex_dir.join("d8.norm.txt");
        if !skotch_norm.exists() || !d8_norm.exists() {
            continue;
        }
        let skotch_text = std::fs::read_to_string(&skotch_norm).unwrap();
        let d8_text = std::fs::read_to_string(&d8_norm).unwrap();
        if !has_main_v_method(&skotch_text) {
            failures.push(format!("{name}: skotch.norm.txt has no main()V method"));
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
