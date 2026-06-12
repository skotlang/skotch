//! End-to-end byte-identity: `skotch d8 <class>` vs real d8 8.10.9.

use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    // Reuse skotch-dex's committed d8 goldens + skotch-classfile inputs.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../skotch-dex/tests/fixtures")
}

/// A battery of straight-line methods (getters, setters, arithmetic with
/// lit-folding, constants of every size, static/instance field access, void
/// calls) — the subset the bootstrap dexer supports — must be byte-identical
/// to real d8.
#[test]
fn straightline_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("B.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("B.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-B-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "B battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

#[test]
fn empty_class_end_to_end_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Empty.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Empty.d8.dex")).unwrap();

    if produced != golden {
        std::fs::write("/tmp/skotch-empty-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "produced {} bytes vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}
