//! ACYCLIC methods that combine local stores with branches (the common shape of
//! real-world code — `max3`, `clamp`, `abs`, conditional assignment). These bail in
//! the bootstrap CFG path ("stores + control flow need full register allocation")
//! and now route to the SSA pipeline, byte-identical to d8.
//!
//! Three pieces made it work (see task #19):
//!  1. PRECISE SSA interference in φ-coalescing — an acyclic merge-φ's operands `x`
//!     and `lo` (both live at the `if(x<lo)`) interfere and stay in distinct
//!     registers, whereas loop-φ operands (init vs back-edge, different blocks) still
//!     coalesce.
//!  2. PRECISE (hole-bearing) live ranges in the linear scan — a fresh value reuses
//!     the register of an arg/value that is DEAD in the block where the new value is
//!     defined (a coarse [min,max] span would falsely keep it busy across the hole).
//!  3. Per-edge return tail-duplication that absorbs a pending φ-move
//!     (`r = hi; return r` → `return hi`).
//!
//! Bailed (construct not yet handled): a partially-dead initializer d8 SINKS into
//! its surviving branch (`int r = 0; if (c) r = …; return r;` — const sinking),
//! φ-move parallel-copy cycles (variable swaps), φ-moves on branching edges
//! (edge-splitting), and `ifnull`/`ifnonnull` in the stack simulator.
//!
//! Posture (same as the loop path): the SSA path NEVER miscompiles (precise
//! interference) and is byte-identical for the engineered/common shapes here.
//! Higher-complexity shapes can still be CORRECT-but-divergent where d8 applies
//! coalescing/move policies we haven't replicated (e.g. d8 declines to coalesce two
//! `aget` results that we do, or sinks a `move` differently) — those aren't asserted
//! byte-identical here.

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

fn assert_byte_identical(name: &str) {
    let produced = dex(name).unwrap_or_else(|e| panic!("{name} should dex: {e}"));
    let golden = std::fs::read(fixtures().join(format!("{name}.d8.dex"))).unwrap();
    if produced != golden {
        std::fs::write(format!("/tmp/skotch-{name}-produced.dex"), &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "{name}: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// `max3` / `clamp` / `absv` / `pick` — conditional updates whose results coalesce
/// with their args (the precise φ-coalescer keeps interfering operands apart).
#[test]
fn acyclic_conditional_updates_byte_identical() {
    assert_byte_identical("AcycB");
}

/// `sign` (if/else-if/else, fresh-const results reusing a dead arg's register via
/// precise interval holes), `ternary`, `negPath` (both arms reassign, dead init
/// DCE'd), `accumIf` (chained `s += …` in-place) — all byte-identical to d8.
#[test]
fn acyclic_fresh_results_byte_identical() {
    for name in ["Sign", "Ternary", "NegPath", "AccumIf"] {
        assert_byte_identical(name);
    }
}

/// `min2`, `absDiff` (compute-then-conditionally-negate), `multiAcc` (straight-line
/// accumulate then a guarding `if`) — more conditional-update shapes, byte-identical.
#[test]
fn acyclic_more_shapes_byte_identical() {
    for name in ["Min2", "AbsDiff", "MultiAcc"] {
        assert_byte_identical(name);
    }
}

/// Null-comparison branches: `ifnull`/`ifnonnull` → `if-eqz`/`if-nez` on an object
/// (ubiquitous in real code). `NullCoal` (`r = a; if (a == null) r = b`) +
/// `NotNullAcc` (`if (a != null) s += a.length`) — byte-identical.
#[test]
fn null_check_branches_byte_identical() {
    for name in ["NullCoal", "NotNullAcc"] {
        assert_byte_identical(name);
    }
}

/// Operand-register reuse (d8's 2addr hint): a binop result reuses a dying operand's
/// register rather than the lowest free one. `AddBr` (`if (c > 0) r = a + b`) +
/// `SibE` (`for(i) s += i; return s + e`) — both previously fresh-register-divergent,
/// now byte-identical.
#[test]
fn operand_register_reuse_byte_identical() {
    for name in ["AddBr", "SibE"] {
        assert_byte_identical(name);
    }
}

/// Algebraic-identity constant folding (matches d8): `s+0`, `s-0`, `s|0`, `s^0`,
/// `s*1`, `s<<0` all fold to `s`, and the now-dead const is DCE'd. `CFold` exercises
/// all six (each `s OP e` where `e` is a propagated constant 0 or 1).
#[test]
fn constant_folding_byte_identical() {
    assert_byte_identical("CFold");
}

/// A partially-dead initializer (`int r = 0; if (c) r = …; return r;` /
/// `String s = "no"; …`) that d8 SINKS into the surviving branch now DEXES (we don't
/// sink — we materialize the const before the `if` and flow it via its register / a
/// φ-move on the merge edge; correct, just not d8's byte-identical sunk shape). The
/// const-sink bail was a byte-identity guard, dropped in functional-correctness mode.
/// Correctness PROVEN on ART by tests/art/ArtConstSink (these shapes → `6,0,yes,no,10,1`).
#[test]
fn partially_dead_const_now_dexes() {
    for name in ["Nested", "Compound", "StrSel"] {
        let dex = dex(name).unwrap_or_else(|e| panic!("{name}: partially-dead const should dex now: {e}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}
