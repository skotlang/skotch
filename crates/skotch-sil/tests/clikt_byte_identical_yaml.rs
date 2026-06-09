//! Forcing-function test: parsing `skotch/parity/.../CliktTesting.kt`
//! must produce YAML byte-identical to the reference at
//! `~/Desktop/psi.yaml` (the output of `scripts/kotlin-psi.main.kts`).
//!
//! The reference YAML was generated with the file:
//! ```
//! file: "skotch/parity/100-clikt/.checkout/5.1.0/clikt-mordant/src/commonMain/kotlin/com/github/ajalt/clikt/testing/CliktTesting.kt"
//! ```
//! so this test passes the same display path to `parse_sil`.

use skotch_sil::{emit_yaml, parse_sil};
use std::path::PathBuf;

const DISPLAY_PATH: &str =
    "skotch/parity/100-clikt/.checkout/5.1.0/clikt-mordant/src/commonMain/kotlin/com/github/ajalt/clikt/testing/CliktTesting.kt";

const SOURCE_REL: &str =
    "../../parity/100-clikt/.checkout/5.1.0/clikt-mordant/src/commonMain/kotlin/com/github/ajalt/clikt/testing/CliktTesting.kt";

fn ref_yaml_path() -> Option<PathBuf> {
    // The reference YAML lives at ~/Desktop/psi.yaml (per the user's
    // session). If $HOME isn't set or the file is missing we skip
    // the test rather than fail an empty workspace.
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Desktop/psi.yaml"))
}

fn source_path() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest).join(SOURCE_REL)
}

#[test]
fn yaml_byte_identical_with_reference_psi_yaml() {
    let Some(ref_path) = ref_yaml_path() else {
        eprintln!("skip: HOME not set");
        return;
    };
    let Ok(expected) = std::fs::read_to_string(&ref_path) else {
        eprintln!(
            "skip: reference YAML at {} not available",
            ref_path.display()
        );
        return;
    };

    let src = std::fs::read_to_string(source_path()).expect("read CliktTesting.kt");
    let tree = parse_sil(DISPLAY_PATH, &src);
    let actual = emit_yaml(&tree);

    if actual == expected {
        return;
    }

    // Surface a tight, actionable diff on the first divergence so the
    // failure message itself drives the next grammar fix.
    let a_lines: Vec<&str> = actual.lines().collect();
    let e_lines: Vec<&str> = expected.lines().collect();
    let common = a_lines.len().min(e_lines.len());
    let first_diff = (0..common).find(|&i| a_lines[i] != e_lines[i]);

    match first_diff {
        Some(i) => {
            let lo = i.saturating_sub(3);
            let hi_a = (i + 8).min(a_lines.len());
            let hi_e = (i + 8).min(e_lines.len());
            let win_a = a_lines[lo..hi_a].join("\n");
            let win_e = e_lines[lo..hi_e].join("\n");
            panic!(
                "YAML differs at line {}:\n\n--- actual (skotch-sil) ---\n{}\n\n--- expected (psi.yaml) ---\n{}\n\n(actual={} lines, expected={} lines)",
                i + 1,
                win_a,
                win_e,
                a_lines.len(),
                e_lines.len(),
            );
        }
        None => panic!(
            "YAML prefix matches but lengths differ: actual={} lines, expected={} lines",
            a_lines.len(),
            e_lines.len()
        ),
    }
}
