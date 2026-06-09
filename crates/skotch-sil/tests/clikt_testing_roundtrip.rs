//! End-to-end test for the SIL pipeline against a real-world Kotlin
//! source file from the clikt parity fixture.
//!
//! The test asserts the load-bearing invariant: parse → emit YAML →
//! re-parse YAML → reconstruct must produce the original source byte
//! for byte. If this passes, every grammar production the file uses
//! is lossless under SIL.

use skotch_sil::{emit_yaml, parse_sil, parse_yaml, reconstruct_from_yaml};

const CLIKT_TESTING_PATH: &str =
    "../../parity/100-clikt/.checkout/5.1.0/clikt-mordant/src/commonMain/kotlin/com/github/ajalt/clikt/testing/CliktTesting.kt";

fn load_source() -> Option<String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let path = std::path::Path::new(&manifest).join(CLIKT_TESTING_PATH);
    std::fs::read_to_string(&path).ok()
}

#[test]
fn clikt_testing_kt_roundtrips_through_sil() {
    let Some(src) = load_source() else {
        // The clikt fixture isn't always checked out — skip when it's
        // missing rather than failing the test suite on a fresh
        // workspace.
        eprintln!("skip: CliktTesting.kt fixture not available");
        return;
    };
    let normalized = if src.contains('\r') {
        src.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        src.clone()
    };

    let tree = parse_sil("CliktTesting.kt", &src);
    let reconstructed = tree.reconstruct();
    assert_eq!(
        reconstructed.len(),
        normalized.len(),
        "byte length differs after parse+reconstruct"
    );
    assert_eq!(
        reconstructed, normalized,
        "byte content differs after parse+reconstruct"
    );
}

#[test]
fn clikt_testing_kt_full_yaml_roundtrip() {
    let Some(src) = load_source() else {
        eprintln!("skip: CliktTesting.kt fixture not available");
        return;
    };
    let normalized = if src.contains('\r') {
        src.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        src.clone()
    };

    let tree = parse_sil("CliktTesting.kt", &src);
    let yaml = emit_yaml(&tree);
    let reparsed = parse_yaml(&yaml).expect("YAML re-parse");
    assert_eq!(reparsed.file, tree.file);
    assert_eq!(reparsed.source_length, tree.source_length);
    assert_eq!(
        reparsed.crlf_normalized, tree.crlf_normalized,
        "crlf flag changed across YAML roundtrip"
    );

    let final_text = reconstruct_from_yaml(&yaml).expect("reconstruct from yaml");
    assert_eq!(
        final_text, normalized,
        "YAML roundtrip lost source bytes"
    );
}

#[test]
fn clikt_testing_yaml_is_idempotent() {
    let Some(src) = load_source() else {
        eprintln!("skip: CliktTesting.kt fixture not available");
        return;
    };

    let tree = parse_sil("CliktTesting.kt", &src);
    let yaml1 = emit_yaml(&tree);
    let tree2 = parse_yaml(&yaml1).expect("re-parse");
    let yaml2 = emit_yaml(&tree2);
    assert_eq!(yaml1, yaml2, "YAML emit is not idempotent under reparse");
}
