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

/// Long/float/double comparisons in branches: `if(longA >= longB)` →
/// `cmp-long v0, v1, v3; if-ltz v0` (args high, cmp result fresh in v0), and
/// `if(floatA < floatB)` → `cmpg-float v0, v0, v1; if-gez v0` (narrow cmp reuses
/// the operand register).
#[test]
fn wide_compare_branch_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Wcmp.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Wcmp.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Wcmp-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Wide-compare branch battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Negation (`neg-int`/`neg-long`, incl. `-(a+b)`) and a 3-argument static call
/// (`invoke-static {v0,v1,v2}` — the 35c form with three register operands).
#[test]
fn neg_and_multiarg_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Misc.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Misc.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Misc-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Neg/multiarg battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Integer/long division and remainder: `div-int/2addr`, `rem-int/2addr`,
/// `div-long/2addr`, and `a/7` → `div-int/lit8` (div/rem DO lit-fold, with the
/// literal as the right operand). Integer div/rem are throwing instructions.
#[test]
fn div_rem_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Div.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Div.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Div-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Div/rem battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Casts and `instanceof`: `(String)o` → `check-cast v0, String` (in place),
/// `(int[])o` → `check-cast v0, [I`, `o instanceof String` → `instance-of v0, v0,
/// String`. Exercises type-reference operands and array-class descriptors.
#[test]
fn cast_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Cast.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Cast.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Cast-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Cast battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Array operations: `aget`/`aput` (`a[i]`), `array-length`, `new-array`,
/// `aget-wide` (long[] element). `firstL` (long[] → long result) exercises the
/// wide-pair straddle rule: the wide result can't reuse a register pair that
/// crosses the args/locals boundary, so it lands in `v0:v1` (both locals).
#[test]
fn array_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Arr.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Arr.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Arr-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Array battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Object allocation: `new X()` → `new-instance v0, X; invoke-direct {v0,..},
/// X.<init>; return-object v0` (the `new`+`dup`+`<init>` idiom, with the
/// constructor initializing the object in place so its register survives to the
/// return). Covers no-arg and one-arg constructors.
#[test]
fn object_alloc_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("New.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("New.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-New-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Object alloc battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Higher-pressure mixes: `f4=(a+b)+(c+d)` (4 args), `chain=((a+1)*3-2)|4`
/// (`x-const` folds to `add-int/lit8 x,-const`), `fieldArith=a*1000+7`
/// (`mul-int/lit16`), `twoConst=(a|BIG)+(a&BIG)` (the result reuses the dead
/// constant's register, and `a` is shared across two terms → args-high v2).
#[test]
fn pressure_mix_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Press2.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Press2.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Press2-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Pressure mix battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Multi-temp nested expressions exercising d8's commutative coalescing:
/// `(a+b)*(a+c)` → `add-int/2addr v1, v0` (the a+b result reuses the DEAD operand
/// `b`'s register because `a` is still live), plus `a*a+b*b` and `(a+b)|(b+c)`.
#[test]
fn nested_expr_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Stress.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Stress.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Stress-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Nested expr battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Instance method calls (`invoke-virtual {v0}, getN; move-result`) and combined
/// field+argument pressure: `addN(k){ n + k }` → `iget v0,v1; add-int/2addr v0,v2`
/// (this→v1, k→v2 args-high, temp→v0). Exercises the allocator on real receiver
/// calls and a three-register layout.
#[test]
fn instance_call_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Q.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Q.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Q-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Instance call battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Straight-line arithmetic with a large (non-lit-foldable) constant forces a
/// scratch register while the argument is live, so d8 relocates the argument
/// high: `addBig(a){a+1000000}` → `const v0,#…; add-int/2addr v1,v0; return v1`.
/// Covers int and wide (long) pressure through the register remap.
#[test]
fn arith_pressure_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Press.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Press.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Press-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Arith pressure battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Instance fields + methods: `get(){return x}` → `iget v0, v1` (receiver high,
/// loaded value low — d8 does NOT coalesce the result into the receiver), plus a
/// field-storing constructor and setter. Exercises args-high allocation on the
/// straight-line path through the register remap.
#[test]
fn instance_field_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("P.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("P.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-P-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Instance field battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Static method invocations: `invoke-static {}`/`{v0}` + `move-result`, with the
/// returned value coalescing into a dead constant-argument register (`viaH`:
/// `const v0,#5; invoke {v0}; move-result v0`), and a two-call chain.
#[test]
fn static_invoke_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Call.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Call.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Call-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Static invoke battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Float/double/long literal constants exercise every const form: `const`/`const/4`
/// for floats (float bits via the narrow path), `const-wide`, `const-wide/16`, and
/// `const-wide/high16` for longs/doubles.
#[test]
fn literal_const_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Lit.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Lit.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Lit-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Literal const battery: produced {} vs golden {}; first diff {:?}",
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
/// the constant while `a` is live, so d8 relocates `a` to the high register
/// (registers=2, ins=1: `a`→v1, const→v0, return values→v1). The allocated→real
/// register remap (d8's args-high placement) reproduces this byte-for-byte.
#[test]
fn args_high_pressure_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArgsHigh.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("ArgsHigh.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-ArgsHigh-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "args-high: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
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

/// Loops through the SSA/φ + linear-scan pipeline (the d8 IR path). `count`
/// (`while (c < n) c++`) is a one-variable loop → `const/4 v0; if-ge v0,v1;
/// add-int/lit8 v0,v0,#1; goto; return v0` (the counter coalesced to v0, the
/// iinc constant rematerialized). `sumTo` (`for (i) s += i`) is a two-variable
/// loop → d8 gives the counter `i` the low register (v0) and the accumulator `s`
/// v1, because it creates φ-nodes in first-read order. The full `.dex` (both
/// loop methods plus the straight-line `<init>`) must be byte-identical to d8.
#[test]
fn loop_battery_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Loop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Loop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Loop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// A loop containing a static method call: `sumTwice(n){ for(i) s += twice(i) }`.
/// Exercises the SSA path's invoke emission (invoke-static {v0} + move-result),
/// `outs_size`, the method-ref Fixup (patched to a pool index by the writer), and
/// debug-position emission (the call is a throwing instruction → `0x0004 line=3`).
/// `twice` (acyclic) goes through the bootstrap path. Full `.dex` byte-identical.
#[test]
fn call_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Calls.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Calls.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Calls-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Call-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// More call shapes in loops: `twoCalls` mixes a value-returning static call
/// (`triple(i)` → move-result) with a VOID static call (`noop(s)` → no
/// move-result), both on one source line so their two throwing positions dedup to
/// one debug entry. `viaInstance` does an instance call (`invoke-virtual {v3,v0},
/// c.scale` — receiver + arg, outs=2). Exercises void/instance invokes + position
/// dedup through the SSA path. Full `.dex` byte-identical.
#[test]
fn void_and_instance_calls_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Calls2.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Calls2.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Calls2-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Void/instance call battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Field access in loops through the SSA path: `sumField` (`iget` instance read),
/// `store` (`iput` instance write), `bumpStatic` (`counter += 1` → `sget` +
/// add-int/lit8 + `sput`, with the loaded value and stored value sharing a
/// register via interval reuse). All field accesses are throwing → debug
/// positions; the field-ref words are fixups. Full `.dex` byte-identical.
#[test]
fn fields_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Fields.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Fields.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Fields-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Fields-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Array access in loops through the SSA path: `sumArr` (`aget` element read),
/// `fillSquares` (`mul-int` + `aput` write), `total` (`array-length` recomputed in
/// the loop header — its result and the later `aget` result share a register via
/// interval reuse). All array ops are throwing → debug positions. Byte-identical.
#[test]
fn arrays_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArrLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("ArrLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-ArrLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Arrays-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Wide (long/double) arithmetic + conversions in loops: `sumLongs` (long
/// accumulator + `aget-wide` + `add-long/2addr` + `return-wide`), `countUp`
/// (`int-to-long` conversion + add-long), `scale` (double accumulator +
/// `add-double/2addr`). Exercises wide constants, the WIDE-first register
/// allocation (the long/double accumulator takes the lowest register PAIR, v0:v1,
/// even though the int counter is read first), and register-pair handling through
/// the args-high remap. Full `.dex` byte-identical.
#[test]
fn wide_arith_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("WideLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("WideLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-WideLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Wide-arith-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Object allocation in/around loops: `iota` (`new-array v0, v2, [I` + aput +
/// `return-object`) and `build` (`new-instance StringBuilder` + invoke-direct
/// `<init>` + a loop calling `sb.append(i)` whose StringBuilder result is DISCARDED
/// — no move-result — then `sb.length()` whose result IS used). Exercises
/// new-instance/new-array (type fixups), the new+dup+<init> idiom, discarded call
/// results (pop), and return-object. Full `.dex` byte-identical.
#[test]
fn new_instance_and_array_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("NewLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("NewLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-NewLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "New-instance/array battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// String constants in loops: `useStr` (`c += "abc".length()`) emits
/// `const-string v2, "abc"` (a string-ref fixup, throwing in d8's model → a debug
/// position) + invoke-virtual length. Exercises ldc resolution + const-string.
/// Full `.dex` byte-identical.
#[test]
fn const_string_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("StrLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("StrLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-StrLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Const-string-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Integer div/rem in loops (throwing → debug positions): `sumDiv` (`n / i` →
/// 3-addr `div-int`), `halves` (`i / 2` → `div-int/lit8`), `mods` (`n % i` →
/// `rem-int`). `sumDiv`/`mods` use `for(i=1;...)`, so the loop-var inits DIFFER
/// (s=0, i=1) → d8 keeps the accumulator in the low register (v0); this validates
/// the GVN-identical-inits φ-ordering rule. Full `.dex` byte-identical.
#[test]
fn int_div_rem_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("DivLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("DivLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-DivLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Int-div/rem-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Forward branches INSIDE a loop body: `condAdd` (`if (i>2) s += i` — a
/// conditional in-place update; the merge-φ for `s` coalesces, no move) and
/// `ifElse` (`if (i>2) s += i; else s -= i` — both arms update `s` in place,
/// merge-φ coalesces). Exercises an intra-loop if-merge φ + the forward
/// if/goto branch fixups. Full `.dex` byte-identical.
#[test]
fn forward_branch_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("IfLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("IfLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-IfLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Forward-branch-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// A ternary with constant arms inside a loop: `int x = (i>0) ? 1 : 2; s += x`.
/// The ternary result lives on the OPERAND STACK across the branch merge, so this
/// exercises stack-merge φs (modeling the operand stack as SSA values). Both const
/// arms rematerialize into the shared φ register (`const v2,#1` / `const v2,#2`) —
/// the stack-φ coalesces, no move. Full `.dex` byte-identical.
#[test]
fn const_ternary_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("TernLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("TernLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-TernLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Const-ternary-in-loop battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// `break` and `continue` inside a loop: `withBreak` (`if(i>5) break`) and
/// `withContinue` (`if(i>5) continue`). These are forward branches out of / to the
/// loop's back-edge region; the merge-φ for `s` coalesces. Full `.dex`
/// byte-identical.
#[test]
fn break_continue_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("BreakLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("BreakLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-BreakLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Break/continue battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Compound conditions and nesting inside a loop: `andCond` (`if(i>2 && i<8)` —
/// short-circuit &&), `orCond` (`if(i<2 || i>8)` — short-circuit ||), `nestedIf`
/// (`if(i>2){ if(i<8) ... }`). All are forward short-circuit branches with a
/// coalescing merge-φ for `s`. Full `.dex` byte-identical.
#[test]
fn compound_conditions_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("CondLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("CondLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-CondLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Compound-conditions battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// A live-through φ-MOVE: `int x = (i>5) ? a : b` where `a`/`b` are live params, so
/// the x merge-φ can't coalesce with either — d8 emits `move v2,v3` (x=a) on one
/// arm and `move v2,v4` (x=b) on the other (each predecessor is single-successor →
/// no edge split). Exercises φ-move insertion + `is_ref`/width move-op selection.
/// Full `.dex` byte-identical.
#[test]
fn phi_move_select_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("SelLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("SelLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-SelLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "φ-move select battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// Two conditionally-selected variables at one merge: `if(i>0){x=a;y=c;}else{...}`.
/// Each edge needs TWO φ-moves (`move v2,v4; move v3,v6` etc.) — validates the
/// multi-move-per-edge path (independent moves, no parallel-copy cycle) in φ-move
/// order. Full `.dex` byte-identical.
#[test]
fn two_phi_moves_per_edge_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("Sel2.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("Sel2.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-Sel2-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "Two-φ-move battery: produced {} vs golden {}; first diff {:?}",
        produced.len(),
        golden.len(),
        (0..produced.len().min(golden.len())).find(|&i| produced[i] != golden[i])
    );
}

/// try/catch inside a loop: `for(...) { try { s += n/i } catch(ArithmeticException e) { s += 1 } }`.
/// Exercises the exception-handler SSA path — a handler-φ snapshots `s` at the
/// throw point (the `div-int`), the dead catch var emits no `move-exception`, and
/// the `try_item` narrows to just the guarded `div-int` (0x0004–0x0006).
#[test]
fn try_catch_in_loop_byte_identical() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("TryLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap();
    let golden = std::fs::read(fixtures().join("TryLoop.d8.dex")).unwrap();
    if produced != golden {
        std::fs::write("/tmp/skotch-TryLoop-produced.dex", &produced).unwrap();
    }
    skotch_dex::validator::validate(&produced).expect("self-validation");
    assert_eq!(
        produced,
        golden,
        "TryLoop: produced {} vs golden {}; first diff {:?}",
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
