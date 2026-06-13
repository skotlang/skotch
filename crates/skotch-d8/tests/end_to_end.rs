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

/// Numeric conversions d8 emits as `conv vDest, vSrc` reusing the source's low
/// register: `i2f`/`i2b`/`i2c`/`i2s`, `l2f`, `f2i`, `d2i`/`d2l`/`d2f`. (The
/// widening forms and `l2i` — which picks the source's high register — diverge
/// and are deliberately excluded; they bail.)
#[test]
fn conversion_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ConvAll.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("ConvAll.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-ConvAll-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Conversion battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Wide (long/double) and float arithmetic: `add-long/2addr`, `mul-long` (3-addr
/// via the mul-bug rule, which applies to long/double too), `add-double/2addr`,
/// `add-float/2addr`, etc. All have registers==ins (no pressure), so the
/// wide-aware binop path must match d8 byte-for-byte.
#[test]
fn wide_arith_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Wide.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Wide.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Wide-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Wide arith battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// `cmp100(int a){ if(a>100) return 1; return 0; }` needs a scratch register for
/// the constant while `a` is live, so d8 relocates `a` to a high register
/// (registers=2, ins=1: `a`→v1, const→v0). The bootstrap doesn't model args-high
/// allocation yet, so it must BAIL — never emit byte-divergent register numbers.
#[test]
fn args_high_pressure_bails_not_miscompiles() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArgsHigh.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let err = dex_classes(&[cf], &opts).expect_err("args-high method must bail, not miscompile");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("args-high") || msg.contains("argument range"),
        "expected an args-high bail, got: {msg}"
    );
}

/// Branch methods that exercise d8's shared-exit return-merging: `absv`
/// (`if(x<0) return -x; return x`) and `clamp0` collapse two same-register
/// returns into one bare `return v0` that the preceding block falls through to.
/// Also includes `sign`/`max2` (no merge) to confirm mixed methods coexist.
#[test]
fn branch_merge_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Br.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Br.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Br-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Br merge battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Conditional-branch methods (`sign`, `max2`, `min2`) — exercises the CFG
/// path: basic-block splitting, local-slot liveness (so `const v0` reuses the
/// argument's register only where it's dead), `if-testz`/`if-test` emission, and
/// branch-offset fixups. All three avoid d8's shared-exit return-merging.
#[test]
fn branch_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Cmp.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Cmp.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Cmp-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Cmp branch battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Like the B battery, but with `two(int a) { int x = a*2; return x+1; }` — a
/// single-assignment local (`istore_1`). d8 coalesces the local into v0
/// (`mul-int/lit8 v0,v0,#2; add-int/lit8 v0,v0,#1; return v0`); the bootstrap
/// dexer must match via its guarded single-assignment local support.
#[test]
fn local_var_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("S.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("S.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-S-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "S local-var battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Two classes dexed into one `classes.dex`. d8's code-layout sort is global
/// across all classes (`holder.toSourceString() + signature`), so every `B`
/// method precedes every `Calc` method (`'B' < 'C'`). This exercises the
/// cross-class ordering that the single-class battery cannot.
#[test]
fn multi_class_battery_byte_identical() {
    let b = skotch_classfile::parse_class_file(&fixtures().join("B.class")).unwrap();
    let calc = skotch_classfile::parse_class_file(&fixtures().join("Calc.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[b, calc], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("BC.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-BC-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "B+Calc multi-class: produced {} vs golden {}; first diff {:?}",
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
