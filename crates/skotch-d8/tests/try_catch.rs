//! try/catch coverage for the SSA exception-handler path.
//!
//! Supported byte-identically (catch variable unused → no `move-exception`):
//!  - try/catch INSIDE a loop — the handler-φ snapshots each guarded local at the
//!    throw point; the `try_item` narrows to the guarded instructions' DEX span.
//!  - ACYCLIC try/catch — via dead-init DCE (the `=0` every path overwrites) +
//!    return tail-duplication (the try block inlines the shared `return`).
//!
//! Deliberately bailed (never miscompiled): a USED catch variable (the handler
//! sits off the loop path, so the exception register can collide with a loop-carried
//! value; d8 also shares the post-catch continuation).

use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../skotch-dex/tests/fixtures")
}

fn dex(name: &str) -> Result<Vec<u8>, String> {
    let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    dex_classes(&[cf], &opts).map_err(|e| format!("{e:#}"))
}

/// `parseLoop` (invoke + aget in the try, multi-instruction try range) and
/// `divLoop` (div in the try) — both dead-catch, both byte-identical to real d8.
#[test]
fn loop_dead_catch_byte_identical() {
    let produced = dex("LoopTryOk").expect("LoopTryOk should dex");
    let golden = std::fs::read(fixtures().join("LoopTryOk.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-LoopTryOk-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "LoopTryOk: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Two throwing ops (`div` + `rem`) inside one try region — the try_item spans
/// both throwing instructions; dead catch.
#[test]
fn loop_two_throwing_ops_byte_identical() {
    let produced = dex("TwoThrows").expect("TwoThrows should dex");
    let golden = std::fs::read(fixtures().join("TwoThrows.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-TwoThrows-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "TwoThrows: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Acyclic try/catch (dead catch): `deadCatch` (div) + `invokeTry` (invoke). Needs
/// dead-init DCE (the `s=0` every path overwrites) + return tail-duplication (the
/// try block inlines the shared `return` instead of `goto`-ing it). Byte-identical.
#[test]
fn acyclic_dead_catch_byte_identical() {
    let produced = dex("AcycTry").expect("AcycTry should dex");
    let golden = std::fs::read(fixtures().join("AcycTry.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-AcycTry-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "AcycTry: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Acyclic void-try-body (`a[i]=1; s=9`) + dead catch — byte-identical.
#[test]
fn acyclic_void_try_byte_identical() {
    let produced = dex("VoidTry").expect("VoidTry should dex");
    let golden = std::fs::read(fixtures().join("VoidTry.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-VoidTry-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(produced, golden, "VoidTry: {} vs {}", produced.len(), golden.len());
}

/// Acyclic try/catch with computation AFTER the merge (`return s * 2`) — the merge
/// block isn't a trivial return, so it stays a real block; still byte-identical.
#[test]
fn acyclic_compute_after_merge_byte_identical() {
    let produced = dex("AfterCompute").expect("AfterCompute should dex");
    let golden = std::fs::read(fixtures().join("AfterCompute.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-AfterCompute-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(produced, golden, "AfterCompute: {} vs {}", produced.len(), golden.len());
}

/// Acyclic USED catch variable (`s = e.hashCode()`) — `move-exception` emitted; the
/// precise-interference allocator keeps the exception register off everything live
/// into the handler. Byte-identical.
#[test]
fn acyclic_used_catch_byte_identical() {
    let produced = dex("Acyc2").expect("Acyc2 should dex");
    let golden = std::fs::read(fixtures().join("Acyc2.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Acyc2-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(produced, golden, "Acyc2: {} vs {}", produced.len(), golden.len());
}

/// A used catch variable IN A LOOP must BAIL: the handler sits on the loop path so
/// the exception register would clobber a loop-carried value, and d8 shares the
/// post-catch continuation — neither modeled yet.
#[test]
fn loop_used_catch_var_bails() {
    let err = dex("TryLoop2").expect_err("loop used catch variable must bail");
    assert!(err.contains("used catch variable in a loop"), "unexpected bail reason: {err}");
}
