//! Byte-identity on real-code-shaped methods (loops + branches + arithmetic +
//! arrays) that exercise the full SSA pipeline end-to-end against d8 8.10.9. These
//! are the kinds of bodies that show up in actual Java/Kotlin libraries.

use skotch_d8::{dex_classes, D8Options, Mode};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../skotch-dex/tests/fixtures")
}

fn assert_byte_identical(name: &str) {
    let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let produced = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name} should dex: {e:#}"));
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

/// `pow` — `long r = 1; for(i) r *= base;` (wide accumulator, long mul-2addr loop).
#[test]
fn pow_byte_identical() {
    assert_byte_identical("Pow");
}

/// `count` — `for(i) if(a[i]==x) c++;` (array length, indexed load, conditional
/// increment inside a loop).
#[test]
fn count_byte_identical() {
    assert_byte_identical("Count");
}

/// `rev` — `while(n>0){ r=r*10+n%10; n/=10; }` (while loop, mul + rem + div).
#[test]
fn rev_byte_identical() {
    assert_byte_identical("Rev");
}

/// A spread of loop shapes that stress the SSA pipeline, all byte-identical to d8:
/// `DoWhile` (do/while), `ArrWrite` (`a[i]=i*2` array store loop), `CondUpd`
/// (`if(i%2==0) s+=i else s-=i` in-loop branch), `DepChain` (`a=b+c; b=a+1; c=a+b`
/// data-dependency chain — NOT a rotation, no sibling-φ), `LongLoop` (`s += x*i`
/// wide accumulator; needs the commutative 3-addr operand-canonicalization
/// `mul-long v3,v3,v5`), `NestedIf` (nested `if` in a loop), `CallLoop`
/// (`s += a[i].length()` method call in a loop).
#[test]
fn loop_shapes_byte_identical() {
    for name in ["DoWhile", "ArrWrite", "CondUpd", "DepChain", "LongLoop", "NestedIf", "CallLoop"] {
        assert_byte_identical(name);
    }
}

/// More loop shapes, all byte-identical: `TernUpd` (`s = c ? s+i : s-i` ternary
/// updating the accumulator), `WhileTrue` (`while(true){ if(c) break; … }`),
/// `NestBoth` (if/else both arms update in a loop), `IdxUpd` (`i += 2` non-unit
/// stride), `ByteLoop` (`s += a[i] & 0xff` — needs lit16 const folding
/// `and-int/lit16 v2,v2,#255`).
#[test]
fn loop_shapes2_byte_identical() {
    for name in ["TernUpd", "WhileTrue", "NestBoth", "IdxUpd", "ByteLoop"] {
        assert_byte_identical(name);
    }
}

/// `RWArr` — `a[i] = a[i] + a[i-1]` (array read+write loop). Exercises sub-const
/// lit-folding: `i-1` has no DEX `sub-int/lit`, so d8 (and now we) emit
/// `add-int/lit8 vDest, vi, #-1`.
#[test]
fn sub_const_fold_byte_identical() {
    assert_byte_identical("RWArr");
}

/// Partial-GVN φ-ordering: a loop with multiple loop variables where only a SUBSET
/// of inits are GVN-identical constants. `TwoAcc` (`s=0,i=1,p=1`) and `TwoCnt`
/// (`x=0; for(i=0,j=n;…)`). d8 shares the identical-init consts and orders those φs
/// by first-read (counter first) while keeping distinct-const groups in source order;
/// matched by grouping inits by value in reorder_entry_inits.
#[test]
fn partial_gvn_phi_order_byte_identical() {
    for name in ["TwoAcc", "TwoCnt"] {
        assert_byte_identical(name);
    }
}

/// Field/char/conditional/ternary loop shapes, byte-identical: `FieldAcc` (instance
/// field bound `i<n`), `CharProc` (`s += x.charAt(i)` String iteration), `LongCond`
/// (`if(a>0 && b>0 || c>0)` short-circuit chain in a loop), `NestTern` (nested
/// ternary `(a>b)?(a>0?1:2):(b>0?3:4)` as a loop accumuland).
#[test]
fn field_char_cond_loops_byte_identical() {
    for name in ["FieldAcc", "CharProc", "LongCond", "NestTern"] {
        assert_byte_identical(name);
    }
}

/// Shapes surfaced by the 5th semantic stress round, all byte-identical to d8:
///  - `FieldMut` (`for(i) acc += i` on an INSTANCE field — read-modify-write of a
///    field across loop iterations inside a virtual method).
///  - `MultiRet` (`if(x<0) return -1; if(x==0) return 0; return 1` — a chain of
///    early returns; exercises return tail-duplication across multiple exits).
///  - `DblCmp` (`for(i<10) if(a<b) s++` — a `double` comparison `cmpg`/`if-*z` driving
///    a conditional increment inside a loop).
///  - `Latent` (`for(i) s ^= (i+5)|(i+7)` — the SAME φ used by TWO lit-foldable adds,
///    both feeding an `or`. A regression guard: the shift form of this shape exposed a
///    register-assignment inconsistency in a never-shipped shift-lit-fold attempt;
///    this proves the multi-use-φ lit-fold path itself is sound).
#[test]
fn stress_round5_byte_identical() {
    for name in ["FieldMut", "MultiRet", "DblCmp", "Latent"] {
        assert_byte_identical(name);
    }
}

/// 10th semantic stress round, byte-identical — more dup-family compound assignments +
/// fresh idioms:
///  - `S10PreInc` (`++a[i]` — array pre-increment).
///  - `S10ArrMul` (`a[i] *= 3`), `S10ArrShr` (`a[i] >>>= 1` — array compound with mul /
///    unsigned-shift).
///  - `S10Fld` (`this.acc += a[i]` — instance-field compound via `dup`).
///  - `S10Idx2D` (`m[i*w+j]++` — array compound with a COMPUTED index under dup2).
///  - `S10ChAr` (`c[i] *= 2` — char-array compound: dup2 + aget-char/aput-char + i2c
///    narrowing; confirms char element ops survive the dup2 path).
///  - `S10Fact` (`long p=1; for(i) p*=i` — wide accumulator factorial).
#[test]
fn stress_round10_byte_identical() {
    for name in ["S10PreInc", "S10ArrMul", "S10ArrShr", "S10Fld", "S10Idx2D", "S10ChAr", "S10Fact"] {
        assert_byte_identical(name);
    }
}

/// Array-element compound assignment, matching d8 — `a[i]++` / `a[i] += x` compile to
/// `aload;iload;dup2;aget;…;aput`, where `dup2` duplicates the array+index so a single
/// load and store share them (the SSA values are reused — no extra instruction). Needed
/// `dup2` (0x5c) in the SSA stack-sim (category-1 form duplicates the top two values;
/// category-2 form duplicates one wide value).
///  - `ArrInc` (`a[i]++` — int array, aget/aput).
///  - `ArrPlEq` (`a[i] += x` — int array).
///  - `ArrLInc` (`a[i] += 2L` — LONG array: dup2 of array+index + aget-wide/aput-wide +
///    add-long, exercising the wide-load path through dup2).
#[test]
fn array_compound_assign_byte_identical() {
    for name in ["ArrInc", "ArrPlEq", "ArrLInc"] {
        assert_byte_identical(name);
    }
}

/// 9th semantic stress round, byte-identical — fresh idioms:
///  - `S9ShMix` (`s += (i<<k) + (i>>2)` — a VARIABLE shift beside a const shift).
///  - `S9Poly` (`r = r*x + a[i]` — Horner polynomial evaluation in a loop).
///  - `S9LArr` (`a[i] = a[i] + i` over a `long[]` — wide array RMW: aget-wide/aput-wide
///    + add-long, with the repeated `a[i]` CSE'd).
///  - `S9CharD` (`v = v*10 + (s.charAt(i) - '0')` — string parse: charAt + char arith).
///  - `S9Mod16` (`s += i%16 + i/256` — remainder/division by powers of two).
///  - `S9Neg` (`s += -a[i] * 2` — unary negation feeding a multiply).
#[test]
fn stress_round9_byte_identical() {
    for name in ["S9ShMix", "S9Poly", "S9LArr", "S9CharD", "S9Mod16", "S9Neg"] {
        assert_byte_identical(name);
    }
}

/// Comparison results take a FRESH register, never a (dead) operand's — matching d8.
/// `cmp-long`/`cmpl`/`cmpg-*` are 23x-only (no 2addr form), so d8 never reuses an
/// operand register for the result (unlike 2addr-able binops). When an operand is a
/// freshly-loaded value that dies at the compare, we used to reuse its register (more
/// compact but divergent); now the result gets a fresh one.
///  - `S7DCmp` (`if(a[i] > t) c++` over `double[]` — a[i] dies at the cmpl-double).
///  - `S8LongCmp` (`if(a[i] == t) c++` over `long[]` — a[i] dies at the cmp-long).
/// (DblCmp, with two live `double` params, was already byte-identical — its operands
/// can't be reused anyway — and stays so.)
#[test]
fn cmp_result_fresh_register_byte_identical() {
    for name in ["S7DCmp", "S8LongCmp"] {
        assert_byte_identical(name);
    }
}

/// 8th semantic stress round, byte-identical — exercising the freshly-activated wide /
/// move-wide paths plus fresh idioms:
///  - `S8WideMv` (`long x=a; for(i) x+=i` — a wide local seeded from a param, exercising
///    a wide move at entry / the move-wide φ-move path).
///  - `S8Wide3` (`long a=0,b=0,c=0` — THREE same-value wide consts; d8 chains the copies
///    `a→b→c`, each move-wide from the previous, matched by picking the most-recent reg).
///  - `S8CharAr` (`c[i]=(char)(c[i]+1)` — char-array read-modify-write → aget/aput-char).
///  - `S8NestArr` (`dst[i]=src[i]` — array-to-array copy loop).
///  - `S8WideAcc` (`s += a[i]*a[i]` over a `double[]` — wide accumulator + double mul,
///    with the repeated `a[i]` CSE'd).
#[test]
fn stress_round8_byte_identical() {
    for name in ["S8WideMv", "S8Wide3", "S8CharAr", "S8NestArr", "S8WideAcc"] {
        assert_byte_identical(name);
    }
}

/// Wide-const sharing, matching d8: two WIDE locals initialized to the same value share
/// ONE materialized const, the second copied via `move-wide` (1 word) instead of
/// re-materialized (`const-wide*` ≥2 words). Done at EMISSION (the const SSA values stay
/// separate so the φ-coalescer keeps the two loop variables in distinct registers —
/// merging them at the SSA level would wrongly coalesce the interfering φs).
///  - `S7Long` / `Wc0` (`long i=0, s=0` → `const-wide v0; move-wide v2,v0`).
///  - `WcA` (`long a=7, b=7` — shares a non-zero value too).
/// (Narrow consts are NOT shared — `const/4` already costs 1 word; see WcMix.)
#[test]
fn wide_const_sharing_byte_identical() {
    for name in ["S7Long", "Wc0", "WcA"] {
        assert_byte_identical(name);
    }
}

/// Mixed narrow+wide loop variables: `WcMix` (`int i=0; long s=0; while(i<n) s+=i`) — a
/// narrow `const/4` counter beside a wide `const-wide` accumulator, with `int-to-long`
/// in the body. Byte-identical to d8 (narrow and wide entry consts are materialized
/// independently — d8 does NOT share a const across widths).
#[test]
fn mixed_width_loop_consts_byte_identical() {
    assert_byte_identical("WcMix");
}

/// `boolean[]` array ops use the BOOLEAN DEX variant, not the byte one. The JVM shares
/// `bastore`/`baload` between `byte[]` and `boolean[]`; DEX splits them
/// (aput-boolean/aget-boolean vs aput-byte/aget-byte) by the array's component type, and
/// emitting the byte variant for a `boolean[]` is an ART VerifyError. Found by stress
/// round 7; fixed by tracing the array operand's declared element type in the SSA path.
///  - `S7AndOr` (`if(i>2 && i<n-2) fl[i]=true` — store → `aput-boolean`).
///  - `S7BoolR` (`if(b[i]) c++` — load → `aget-boolean`).
#[test]
fn boolean_array_ops_byte_identical() {
    for name in ["S7AndOr", "S7BoolR"] {
        assert_byte_identical(name);
    }
}

/// 7th semantic stress round, byte-identical: `S7Cont` (`if(c) continue` in a loop),
/// `S7CharA` (`s += c[i]` char-array load → `aget-char`), `S7MAcc` (two accumulators
/// `s1+=a[i]; s2+=a[i]*2` — the repeated `a[i]` is CSE'd to one load).
#[test]
fn stress_round7_byte_identical() {
    for name in ["S7Cont", "S7CharA", "S7MAcc"] {
        assert_byte_identical(name);
    }
}

/// Store-to-load forwarding, matching d8: a read of a just-STORED location (no
/// intervening store/call) reuses the stored value instead of reloading.
///  - `SfArr` (`a[i]=i*2; s += a[i]` — the `add` reuses the stored value, no reload).
///  - `SfFld` (`this.x=i; s += this.x` — reuses the stored counter).
///  - `SfByte` (`a[i]=(byte)(i*2); s += a[i]` — forwards the ALREADY-narrowed stored
///    value past `aput-byte`; valid bytecode applies `int-to-byte` before the store so
///    the forwarded value is exactly what `aget-byte` would read back).
/// Soundness (also byte-identical — skotch reloads, matching d8's own conservatism):
///  - `SfsCall` (`this.x=i; bump(); s += this.x` — RELOADS after the call: a method
///    may mutate the field, so forwarding would be a miscompile).
///  - `SfsAlias` (`a[i]=i; b[i]=99; s += a[i]` — RELOADS after the second store: `b`
///    may alias `a`, so the cache is invalidated and `a[i]` is re-read).
#[test]
fn store_to_load_forwarding_byte_identical() {
    for name in ["SfArr", "SfFld", "SfByte", "SfsCall", "SfsAlias"] {
        assert_byte_identical(name);
    }
}

/// Block-local redundant-load elimination (CSE / local value numbering), matching d8:
/// a repeated memory read with the same operands and no intervening store/call is
/// collapsed to a single load.
///  - `CseArr` (`s += a[i]*a[i]` — one `aget` then `mul-int v2,v2,v2`, not two loads).
///  - `CseFld` (`s += this.x*this.x` — one `iget` then `mul-int v2,v2,v2`).
/// Soundness (not committed, verified manually): with a store or call BETWEEN the two
/// reads (`a=x; bump(); a+x` / `a=x; x=v; a+x`) skotch correctly RELOADS — the
/// invalidation clears the whole cache on any aput/iput/sput/invoke. (d8 goes further
/// and store-forwards the just-written value; that's a separate opt we don't do, so
/// those shapes are correct-but-divergent, not regressions.)
#[test]
fn redundant_load_cse_byte_identical() {
    for name in ["CseArr", "CseFld"] {
        assert_byte_identical(name);
    }
}

/// 6th semantic stress round, all byte-identical to d8 — real-code idioms probing
/// previously-uncovered shapes:
///  - `S6Rem` (`s += i % 3` — int remainder by a constant → `rem-int/lit8`).
///  - `S6Div` (`s += i / 7` — int division by a constant → `div-int/lit8`).
///  - `S6Hash` (`h = h*31 + a[i]` — the classic hash accumulator: mul-by-const +
///    array load in a loop).
///  - `S6Bit` (`if ((mask & (1<<i)) != 0) c++` — a bit-test with a VARIABLE shift
///    `1<<i` (3-addr shl-int) feeding an `and` and a conditional increment).
///  - `S6Mix` (`acc += (long)a[i] * i` — int→long widening (`int-to-long`) then a long
///    multiply accumulating into a wide φ).
///  - `S6Ladder` (`if(x==1)…else if(x==2)…else if(x==3)…else…` — an if-else-if return
///    ladder; exercises chained compares + tail-duplicated returns, no loop).
///  - `S6Narrow` (`s += (byte)(i*7)` — int→byte narrowing (`int-to-byte`) in a loop).
/// (A nested ternary `i>0?(i>10?2:1):0`, S6Tern, is correct-but-divergent: d8 leaves a
/// dead φ-entry const we DCE, so we emit smaller correct code — not committed.)
#[test]
fn stress_round6_byte_identical() {
    for name in ["S6Rem", "S6Div", "S6Hash", "S6Bit", "S6Mix", "S6Ladder", "S6Narrow"] {
        assert_byte_identical(name);
    }
}

/// Shift-by-constant lit-folding: d8 folds `x << c` / `x >> c` / `x >>> c` (constant
/// amount) into `shl/shr/ushr-int/lit8` (22b), rematerializing the const. Landing this
/// also required regalloc::reg_fields to cover 0xe0–0xe2 so the args-high remap rewrites
/// the shift instructions' register fields (without it the registers desynced into a
/// miscompile — the bug an earlier naive attempt hit and bailed on).
///  - `BitLoop` (`s ^= (i<<2)|(i>>>1)` — ishl + iushr, both reading the SAME φ).
///  - `ShrLoop` (`s += (i>>1)+(i>>3)` — two arithmetic ishr of one φ).
///  - `LongShl` (`s += ((long)i)<<2` — a LONG shift, which has no lit form: must stay
///    3-addr `shl-long`, NOT lit-folded — guards the int-only restriction).
///  - `ShlReg` (`s += i<<k` — a VARIABLE shift amount: stays 3-addr, not folded).
#[test]
fn shift_const_lit_fold_byte_identical() {
    for name in ["BitLoop", "ShrLoop", "LongShl", "ShlReg"] {
        assert_byte_identical(name);
    }
}

/// `Gcd` (`while(b!=0){ t=a%b; a=b; b=t; }`) — the loop header IS the entry block (no
/// statement before the `while`), so the header's only CFG pred was the back-edge and
/// dominance-frontier φ-placement gave the loop variables no φs (they'd never update). It
/// now DEXES correctly: build_ssa synthesizes an empty PRE-HEADER as the new entry, giving
/// the header the entry edge as a second predecessor so its φs are placed, with the
/// argument value as the entry operand. The transform shifts every block index up by one,
/// INCLUDING exception edges (insert_entry_preheader shifts cfg.exc_edges and exc_regions are
/// rebuilt from the raw handler PCs against the shifted blocks), so it produces the same graph a
/// natural pre-header would and works with try/catch too. Correctness on a REAL device for
/// several entry-header loop shapes (Euclid gcd with a sibling-φ swap, a nested-if collatz,
/// plain count-down) is proven by `tests/art/ArtPreheader`, and for entry-header loops with a
/// try/catch in the body (typed catch + whole-body div-by-zero catch, accumulator threaded as
/// an arg) by `tests/art/ArtPreheaderTry`; here: dexes + self-validates. NOTE: `Fib`/`ArtPhiCycle`
/// (back-edge parallel copies) graduated earlier — see `sibling_phi_parallel_copy_now_dexes`.
#[test]
fn loop_header_is_entry_now_dexes_via_preheader() {
    for name in ["Gcd", "ArtPreheader", "ArtPreheaderTry"] {
        let path = if name.starts_with("Art") {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/art/{name}.class"))
        } else {
            fixtures().join(format!("{name}.class"))
        };
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: loop-header-is-entry should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// >16-register support: a binop whose register doesn't fit the compact 4-bit `/2addr` nibble
/// form now emits the 3-address form (8-bit fields) instead, where both the allocated and the
/// remapped register fit. `ArtManyRegs.combine` has 20 live locals; its add chain previously
/// TRUNCATED `add-int/2addr v0, v16` into `add v0, v0` at emit time (the 1500-vs-1430 miscompile)
/// and so had to bail. It now DEXES correctly via widening. Real-device correctness is proven by
/// `tests/art/ArtWideArith`; ArtManyRegs itself was verified on ART (combine(0)=1430, combine(3)=2300).
#[test]
fn over_16_registers_arith_now_dexes_via_widening() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtManyRegs.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtManyRegs (>16 registers) should now dex via widening");
    skotch_dex::validator::validate(&dex).expect("ArtManyRegs dex must validate");
}

/// A >16-register method whose high registers land in a nibble form with NO wider alternative
/// still BAILS — never miscompiles. `ArtWideLoop.run` has 16 loop-carried accumulators (>16
/// registers); its `t < n` loop comparison is an `if-test` (22t, nibble) that has no 8-bit form
/// to widen to and isn't yet spilled, so dexing bails loudly (the per-site nibble guard /
/// `remap_insns` fail-not-truncate). This preserves the never-truncate guarantee for the >16
/// cases this slice does not yet cover (unwidenable nibbles: if-test/iget/iput/array-length/...).
#[test]
fn over_16_registers_unwidenable_nibble_still_bails() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtWideLoop.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    dex_classes(&[cf], &opts).expect_err("ArtWideLoop (>16 regs, unwidenable if-test) must bail, not miscompile");
}

/// A >16-register method calling a multi-arg method with its own PARAMETERS as arguments: the
/// params have LOW allocated registers but the args-high remap moves them to HIGH final registers,
/// so the compact 35c invoke form passes the allocated check yet would overflow its 4-bit arg
/// nibble after remap. `emit_invoke` now selects the invoke/range form on the FINAL register (not
/// just the allocated one), so `ArtWideCall.run` DEXES instead of bailing. Real-device correctness
/// is proven by `tests/art/ArtWideCall` (276/5916/-1098). (Verified to BAIL when the final-register
/// check is removed — the allocated-only trigger left it as 35c and `remap_insns` then bailed.)
#[test]
fn over_16_registers_invoke_args_use_range_via_final_check() {
    let cf = skotch_classfile::parse_class_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtWideCall.class"),
    )
    .unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtWideCall (>16 regs, param args) should dex via range form");
    skotch_dex::validator::validate(&dex).expect("ArtWideCall dex must validate");
}

/// >16-register `iget`/`iput` (22c, nibble-only register fields) now DEX rather than bail: the
/// dexbuilder reserves 2 low scratch registers and routes any operand whose FINAL register is ≥16
/// through them via `move(-object)/from16`. The object operand always moves with
/// move-object/from16 (0x08) — never move/from16 (0x02), which ART rejects as `copy1 …
/// type=Reference`. Runtime correctness on a REAL device is proven by `tests/art/ArtSpill*`
/// (ArtSpillThis 2153/338/105 int iget+dest reload; ArtSpillObj 158/240/126 iget-object dest
/// reload via 0x08; ArtSpillPut 153/238/119 iput obj-high; ArtSpillLoop 3153/388/98 iget in a
/// loop, spill inserted across the back-edge with offsets intact). Here: each dexes + self-validates.
#[test]
fn over_16_registers_iget_iput_now_spill_through_scratch() {
    let art = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art");
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    for name in ["ArtSpillThis", "ArtSpillObj", "ArtSpillPut", "ArtSpillLoop"] {
        let cf = skotch_classfile::parse_class_file(&art.join(format!("{name}.class"))).unwrap();
        let dex = dex_classes(&[cf], &opts)
            .unwrap_or_else(|e| panic!("{name} (>16-reg iget/iput) should dex via scratch spill: {e:#}"));
        skotch_dex::validator::validate(&dex)
            .unwrap_or_else(|e| panic!("{name} spill dex must validate: {e:#}"));
    }
}

/// A NON-CAPTURING lambda (`invokedynamic` → `LambdaMetafactory.metafactory`, no captured args,
/// static impl, non-generic SAM) is desugared d8-style: a SYNTHETIC class implementing the
/// functional interface is generated (singleton INSTANCE + a SAM method forwarding to the impl),
/// and the call site becomes `sget-object INSTANCE`. ArtLambda exercises Runnable (void SAM),
/// IntUnaryOperator (int→int), and IntBinaryOperator (two int args); dexing it produces the user
/// class PLUS three synthetic lambda classes. Correctness on a REAL device is proven by
/// `tests/art/ArtLambda`; here: dexes (incl. the synthetics) + self-validates. Capturing lambdas,
/// non-static impls, and generic/bridge SAMs still bail (never miscompile).
#[test]
fn lambda_metafactory_non_capturing_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambda.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambda: non-capturing lambda should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambda: invalid dex: {e:#}"));
}

/// A CAPTURING lambda (`invokedynamic` → LambdaMetafactory whose indy descriptor has dynamic
/// args = the captured values) is desugared to a synthetic class with one instance field per
/// capture, a constructor storing them, and a SAM method that loads `this.f$N` then invokes the
/// static impl with `captures ++ SAM-params`. The call site becomes `new-instance + invoke-direct
/// <init>(captures)`. ArtLambdaCapture captures an int, two ints, and a String ref (all via the
/// non-generic IntUnaryOperator SAM), and also dispatches through a helper that takes the
/// interface as a parameter — which exercises the invokeinterface bootstrap path (a pre-existing
/// `0xb9 → 0x74` mis-mapping, now `0x72`). Correctness on a REAL device is proven by
/// `tests/art/ArtLambdaCapture`; here: dexes (incl. synthetics) + self-validates. Instance-capture
/// / method-ref / generic-bridge lambdas still bail (never miscompile).
#[test]
fn lambda_metafactory_capturing_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaCapture.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaCapture: capturing lambda should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaCapture: invalid dex: {e:#}"));
}

/// A GENERIC functional interface (e.g. `Function<String,Integer>`) has an ERASED SAM
/// (`apply(Object)Object`) while the lambda impl uses the instantiated types (`(String)Integer`).
/// The synthetic class implements the erased abstract method but check-casts each generic
/// reference parameter down to its instantiated type before calling the impl; the covariant
/// reference return needs no cast. Works for non-capturing AND capturing generic lambdas, and for
/// non-Function SAMs (Predicate). ArtLambdaGeneric covers Function<String,Integer> (non-capturing
/// + capturing), Predicate<String>, and Function<Integer,Integer>. Correctness on a REAL device is
/// proven by `tests/art/ArtLambdaGeneric`; here: dexes (incl. synthetics) + self-validates.
#[test]
fn lambda_metafactory_generic_sam_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaGeneric.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaGeneric: generic SAM lambda should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaGeneric: invalid dex: {e:#}"));
}

/// METHOD REFERENCES (`Type::method`) are invokedynamic+LambdaMetafactory like lambdas, but the
/// impl method-handle kind selects the forwarding invoke: 6=static, 5=invokevirtual,
/// 9=invokeinterface, 7=invokespecial. For an UNBOUND instance reference (`String::isEmpty` as
/// `Predicate<String>`) the first instantiated SAM parameter is the receiver, so the synthetic SAM
/// forwards all (cast) params via the right invoke (the impl descriptor omits the receiver).
/// ArtMethodRef covers kind-5 (String::toUpperCase/isEmpty/trim), kind-6 (static
/// ArtMethodRef::square), and kind-9 (List::isEmpty), all boxing-free. Correctness on a REAL device
/// is proven by `tests/art/ArtMethodRef`; here: dexes (incl. synthetics) + self-validates. Boxing
/// adaptations, bound/instance-capturing refs, and constructor refs (kind 8) still bail.
#[test]
fn lambda_method_references_now_dex() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtMethodRef.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtMethodRef: method references should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtMethodRef: invalid dex: {e:#}"));
}

/// BOUND instance method references (`sb::toString`, `"key"::equals`, `prefix::startsWith`) capture
/// the receiver as the single dynamic argument; the synthetic class stores it in a field and the
/// SAM `iget`s it then forwards via the instance invoke (invoke-virtual/interface/direct). This
/// reuses the capturing-lambda machinery — the receiver is just the lone capture and the impl's
/// declared params equal the instantiated SAM params. ArtBoundRef covers a kind-5 no-arg supplier
/// (with a mutated captured receiver), a one-arg predicate, and a local-capturing predicate, all
/// boxing-free. Correctness on a REAL device is proven by `tests/art/ArtBoundRef`; here: dexes +
/// self-validates. Constructor refs (kind 8) and boxing adaptations still bail.
#[test]
fn lambda_bound_method_references_now_dex() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtBoundRef.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtBoundRef: bound method references should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtBoundRef: invalid dex: {e:#}"));
}

/// RETURN-BOXING adaptation: when a method reference's impl returns a primitive but the
/// functional interface's instantiated return is the boxed wrapper (e.g. `String::length` as
/// `Function<String,Integer>` — int → Integer), the synthetic SAM boxes the result with
/// `<Wrapper>.valueOf(prim)` before returning it. ArtLambdaBox covers unbound (String::length),
/// bound ("x"::isEmpty → Boolean), and static (Integer::parseInt) return-box references. Proven on
/// a REAL device by `tests/art/ArtLambdaBox`; here: dexes + self-validates. Parameter-unboxing and
/// wide (Long/Double) boxing still bail.
#[test]
fn lambda_return_boxing_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaBox.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaBox: return-boxing lambda should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaBox: invalid dex: {e:#}"));
}

/// A capturing lambda whose impl is an INSTANCE method — it closes over `this` (plus locals), so
/// javac emits a non-static `lambda$` impl (handle kind 5/9) and the indy captures `this` as the
/// first dynamic argument followed by the other captured values. The synthetic class stores all
/// captures in fields and the SAM `iget`s them then forwards via the instance invoke
/// (`invoke-virtual {f$0(this), f$1.., args}`) — generalizing the bound-method-reference path to
/// >1 capture. This also exercises the fix that classifies a `lambda$` instance method (relaxed
/// from private to package-private so the synthetic class can call it) as VIRTUAL, not direct.
/// ArtCaptureThis covers a this+local capture returning int and a this+String capture returning
/// String. Proven on a REAL device by `tests/art/ArtCaptureThis`; here: dexes + self-validates.
#[test]
fn lambda_instance_impl_capturing_this_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtCaptureThis.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtCaptureThis: this-capturing lambda should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtCaptureThis: invalid dex: {e:#}"));
}

/// CONSTRUCTOR REFERENCES (`ArrayList::new`, `StringBuilder::new`) use method-handle kind 8
/// (newInvokeSpecial). The synthetic SAM has a distinct shape — `new-instance v0, C` ;
/// `invoke-direct {v0, args}, C.<init>(args)V` ; `return-object v0` — so the result IS the freshly
/// constructed object (no impl move-result; the SAM's instantiated return is the constructed
/// class, the handle return is void). ArtCtorRef covers a no-arg `Supplier<ArrayList>`, a 1-arg
/// `Function<String,StringBuilder>`, and a no-arg `Supplier<StringBuilder>`. Proven on a REAL
/// device by `tests/art/ArtCtorRef`; here: dexes + self-validates. Capturing (inner-class) ctor
/// refs still bail.
#[test]
fn lambda_constructor_references_now_dex() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtCtorRef.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtCtorRef: constructor references should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtCtorRef: invalid dex: {e:#}"));
}

/// Lambda metafactory SIGNATURE ADAPTATIONS that don't require (un)boxing: (1) RETURN-TO-VOID — a
/// non-void impl result is discarded when the SAM returns void (`Consumer<String> = list::add`,
/// add returns boolean); (2) COVARIANT CONSTRUCTOR-REF return — the constructed class is a subtype
/// of the SAM's instantiated return (`Supplier<CharSequence> = StringBuilder::new`); (3) COVARIANT
/// PARAMETER widening — the SAM provides a subtype where the (erased) impl parameter is a supertype
/// (the String passed to `add(Object)`). All are reference-safe with no cast. ArtLambdaAdapt covers
/// all three. Proven on a REAL device by `tests/art/ArtLambdaAdapt`; here: dexes + self-validates.
/// Primitive (un)boxing adaptations still bail.
#[test]
fn lambda_signature_adaptations_now_dex() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaAdapt.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaAdapt: signature adaptations should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaAdapt: invalid dex: {e:#}"));
}

/// PARAMETER UNBOXING: when an instantiated SAM parameter is a boxed wrapper (Integer) but the impl
/// wants the primitive (int), the synthetic SAM unboxes it (`<Wrapper>.xxxValue()`) after the
/// check-cast and before the impl invoke. The impl's declared parameter k maps to SAM parameter
/// recv_offset+k (an unbound instance ref's receiver has no impl slot). ArtLambdaUnbox covers a
/// static method ref needing param-unbox + return-box (`::dbl` as Function<Integer,Integer>), a
/// two-argument ref (`::add` as BiFunction), and a Predicate<Integer> lambda. Proven on a REAL
/// device by `tests/art/ArtLambdaUnbox`; here: dexes + self-validates. Wide (long/double) unbox and
/// capturing/bound param-unbox still bail.
#[test]
fn lambda_param_unboxing_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaUnbox.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaUnbox: param unboxing should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaUnbox: invalid dex: {e:#}"));
}

/// WIDE (long/double) lambda SAM parameters and returns. A wide value occupies a register PAIR, so
/// the synthetic SAM tracks each parameter's start register + wide flag (rather than flattening),
/// advances by 2 for wide, and lists both halves of each wide argument in the impl invoke; the wide
/// return uses move-result-wide/return-wide. ArtLambdaWide covers DoubleUnaryOperator (wide
/// param+return), LongUnaryOperator, DoubleBinaryOperator (two wide params), and DoubleSupplier
/// (wide return). Proven on a REAL device by `tests/art/ArtLambdaWide`; here: dexes + self-validates.
/// (Wide captures and >5-word invokes still bail.)
#[test]
fn lambda_wide_params_now_dex() {
    let cf = skotch_classfile::parse_class_file(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtLambdaWide.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtLambdaWide: wide lambda params should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtLambdaWide: invalid dex: {e:#}"));
}

/// A φ whose operand is a SIBLING φ in the same loop header is a parallel copy on the
/// back-edge — a swap (`a=b; b=a`, a 2-cycle), a 3-way rotation, or a chain (`a=b; b=t`,
/// Fibonacci). This used to bail (a naive in-place update read a sibling AFTER it was
/// overwritten — a lost copy / miscompile). It now DEXES: the back-edge's φ-moves are
/// emitted as ONE set through a single `emit_move_list`, which sequentializes the parallel
/// copy — dependency-ordering chains and breaking true cycles with the `registers_used`
/// scratch temp. Correctness on a REAL device (the swap 2-cycle AND the 3-way rotation, run
/// over several iteration counts) is proven by `tests/art/ArtPhiCycle`; here: dexes +
/// self-validates. (`emit_move_list` still BAILS — never miscompiles — on wide cycles and a
/// scratch register ≥ 16.)
#[test]
fn sibling_phi_parallel_copy_now_dexes() {
    for name in ["Fib", "ArtPhiCycle"] {
        let path = if name == "ArtPhiCycle" {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtPhiCycle.class")
        } else {
            fixtures().join(format!("{name}.class"))
        };
        let cf = skotch_classfile::parse_class_file(&path).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: sibling-φ parallel copy should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// Array-literal initializers (`int[] a = {3,1,4,…}`) that used to SILENTLY MISCOMPILE in
/// the bootstrap straight-line path: javac emits `newarray; (dup; idx; val; iastore)*`, and
/// the per-store path `release`d the array register after each store — but the `dup`'d array
/// is still live for the next store, so a following `const` reused the freed register and
/// CLOBBERED the array (`new-array v1,…; aput …,v1,…; const/4 v1,#1; aput …,v1,…` — v1 is no
/// longer the array). It even passed DEX self-validation. d8 never emits per-element stores
/// here: it lowers to filled-new-array (small) or fill-array-data (large, via a
/// value-dependent cost model — e.g. `int[5]`→filled-new-array but `float[5]`→fill-array-data).
/// Array-literal initializers (`int[] a = {…}`) now DEX via the SSA fallback (the
/// bootstrap straight-line path still bails — "needs fill-array-data" — but
/// `dex_method` now retries with the SSA pipeline, which emits the naive
/// newarray+aput sequence; correct, just larger than d8's fill-array-data).
///  - `IArr` (constant `{3,1,4,…}`), `NCArr` (non-constant `{x, x*2, …}`).
/// NOT byte-identical to d8 — correctness is proven by `tests/art/ArtFallback`
/// (a runnable array-literal exercised on ART). Here: dexes + self-validates.
#[test]
fn array_literal_now_dexes_via_ssa_fallback() {
    for name in ["IArr", "NCArr"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: array literal should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// 17th semantic stress round, byte-identical — fresh idioms:
///  - `S17MixBool` (`if(a>0 && (b<0||c==0))` — mixed &&/|| short-circuit chain).
///  - `S17Short` (`s += a[i]` over a `short[]` — aget-short sign-extending load).
///  - `S17Ladder` (`if(x<10)return 0; if(x<100)return 1; …` — if-ladder dispatch).
///  - `S17CharSum` (`x += s.charAt(i) - 'A'` — char arithmetic over a String).
#[test]
fn stress_round17_byte_identical() {
    for name in ["S17MixBool", "S17Short", "S17Ladder", "S17CharSum"] {
        assert_byte_identical(name);
    }
}

/// 16th semantic stress round, byte-identical — fresh idioms:
///  - `S16Mask` (`m |= 1<<i` — bitmask construction via variable shift + or).
///  - `S16Count2` (`if(a[i]<0) neg++` — count negatives).
///  - `S16Pow2` (`while(e>0){ if((e&1)!=0)r*=b; b*=b; e>>=1; }` — fast exponentiation).
///  - `S16StrHash` (`h = 31*h + s.charAt(i)` — String.hashCode-style; validates the
///    const-LEFT commutative lit-fold `mul-int/lit8 h,#31` in a real hash loop).
#[test]
fn stress_round16_byte_identical() {
    for name in ["S16Mask", "S16Count2", "S16Pow2", "S16StrHash"] {
        assert_byte_identical(name);
    }
}

/// SEQUENTIAL loops are no longer mis-detected as NESTED. `has_nested_loop` used to flag
/// `for(i){…} for(j){…}` as nested because the first header dominates the second; the
/// precise check now also requires the inner header to reach a LATCH of the outer (be
/// inside its looping body), which sequential loops don't. `S15TwoLoop` (`for(i)s+=i;
/// for(j)s-=j`) now dexes correctly (self-validates) instead of a false-positive bail.
/// A genuine nested loop `S15Nest` (`for(i)for(j)s+=i*j`) MUST still bail (d8 leaves a
/// dead φ-entry const we'd diverge on). (S15TwoLoop is correct but register-allocation-
/// divergent — we reuse the first counter's register for the second; not byte-identical.)
#[test]
fn sequential_and_nested_loops_now_dex() {
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    // sequential loops: dex + self-validate (no false-positive nested bail).
    let cf = skotch_classfile::parse_class_file(&fixtures().join("S15TwoLoop.class")).unwrap();
    let p = dex_classes(&[cf], &opts).expect("sequential loops must dex, not bail");
    skotch_dex::validator::validate(&p).expect("sequential-loop dex self-validates");
    // genuinely NESTED loops now DEX too (the bail was only to stay byte-identical with
    // d8's un-DCE'd dead const; our smaller output is correct — ART-proven by ArtNested).
    let cf = skotch_classfile::parse_class_file(&fixtures().join("S15Nest.class")).unwrap();
    let p = dex_classes(&[cf], &opts).expect("nested loop must dex now");
    skotch_dex::validator::validate(&p).expect("nested-loop dex self-validates");
}

/// 15th semantic stress round, byte-identical — fresh idioms:
///  - `S15BitRev` (`r=(r<<1)|(n&1); n>>=1` — bit reversal).
///  - `S15Cube` (`s += i*i*i` — chained multiply).
///  - `S15Clamp2` (`Math.max(lo, Math.min(hi, x))` — nested static clamp).
///  - `S15Parity` (`while(n!=0){ p^=1; n&=n-1; }` — bit parity).
///  - `S15AbsM` (`s += Math.abs(a[i])` — Math.abs in a loop).
#[test]
fn stress_round15_byte_identical() {
    for name in ["S15BitRev", "S15Cube", "S15Clamp2", "S15Parity", "S15AbsM"] {
        assert_byte_identical(name);
    }
}

/// Commutative lit-fold with the constant on the LEFT, matching d8: `3*n` → `mul-int/
/// lit8 n,#3`. For a commutative op (add/mul/and/or/xor) d8 folds the constant whether
/// it's the left or right operand (swapping the variable to the source); we previously
/// only folded const-on-the-right.
///  - `ClMul` (`s += 3 * i`), `ClAnd` (`s += 7 & a[i]`), `ClAdd` (`s = 100 + a[i] + s`).
#[test]
fn const_left_commutative_lit_fold_byte_identical() {
    for name in ["ClMul", "ClAnd", "ClAdd"] {
        assert_byte_identical(name);
    }
}

/// 14th semantic stress round, byte-identical — fresh idioms:
///  - `S14Djb2` (`h = h*33 + a[i]` — djb2-style hash; mul-by-const-RIGHT).
///  - `S14Ctz` (`while(n!=0 && (n&1)==0){ n>>=1; c++; }` — count trailing zeros).
///  - `S14Avg` (`for(i) s+=a[i]; return s/a.length` — sum then divide).
///  - `S14Min3` (`Math.min(Math.min(a,b),c)` — nested static calls).
///  - `S14Wrap` (`((x%m)+m)%m` — Euclidean modulo).
#[test]
fn stress_round14_byte_identical() {
    for name in ["S14Djb2", "S14Ctz", "S14Avg", "S14Min3", "S14Wrap"] {
        assert_byte_identical(name);
    }
}

/// 13th semantic stress round, byte-identical — fresh idioms:
///  - `S13DoWhile` (`do { s+=i; i++; } while(i<n)` — do/while loop shape).
///  - `S13Virt` (`s += a[i].hashCode()` — virtual call on an array element in a loop).
///  - `S13Not` (`s += ~a[i]` — bitwise complement, lowered `xor-int/lit8 #-1`).
///  - `S13MathMax` (`m = Math.max(m, a[i])` — kept as a static call; no ternary φ, so the
///    over-coalesce net does not trigger and it's byte-identical).
#[test]
fn stress_round13_byte_identical() {
    for name in ["S13DoWhile", "S13Virt", "S13Not", "S13MathMax"] {
        assert_byte_identical(name);
    }
}

/// 12th semantic stress round, byte-identical — fresh idioms:
///  - `S12Bit2` (`if(((mask>>i)&1)!=0) c++` — bit extraction via variable shift + and).
///  - `S12Tri` (`if(v>0)p++; else if(v<0)n++; else z++` — three-way sign count).
///  - `S12Pow` (`r=1; for(i<e) r*=base` — integer exponentiation).
///  - `S12Dot` (`s += a[i]*b[i]` — dot product over two arrays).
///  - `S12Find` (`for(i) if(a[i]==x) return i; return -1` — linear search w/ early return).
#[test]
fn stress_round12_byte_identical() {
    for name in ["S12Bit2", "S12Tri", "S12Pow", "S12Dot", "S12Find"] {
        assert_byte_identical(name);
    }
}

/// Chained int add/sub-by-constant combined into one, matching d8: `(i+7)-1` → `i+6` when
/// the intermediate is single-use. `CacL` (`s += a[i+7-1] + a[i+2+3]`) emits
/// `add-int/lit8 i,#6` / `add-int/lit8 i,#5` (one add each) — common in array indexing.
/// (Straight-line chains like `return (x+10)+5` go through the bootstrap stack-path, which
/// can't combine without tracking — a separate gap.)
#[test]
fn const_add_combine_byte_identical() {
    assert_byte_identical("CacL");
}

/// Bootstrap (CFG) path: a narrow `cmp*` (float) whose operand is a LIVE LOCAL — re-read on
/// a successor path — must NOT reuse that operand's register for the result, or the compare
/// clobbers a value still needed downstream (a silent miscompile that even passed DEX
/// self-validation). `FloatCmp`/`DCmp3` (`if(a<b) return -1; if(a>b) return 1; return 0`)
/// re-load `a`,`b` in the second compare: d8 puts the first cmp result in a fresh register and
/// keeps the params live, reusing a register only in the SECOND compare where the operands are
/// finally dead. The fix frees a cmp operand register only when the value is dead-after
/// (live-out check); both are now byte-identical (FloatCmp was previously miscompiled). The
/// wide `cmp-long`/`cmp-double` path already took a fresh register and is unchanged.
#[test]
fn cmp_live_operand_not_clobbered_byte_identical() {
    for name in ["FloatCmp", "DCmp3"] {
        assert_byte_identical(name);
    }
}

/// 22nd semantic stress round, byte-identical — fresh control-flow + loop idioms:
///  - `CharLoop` (`for(i) { char ch=s.charAt(i); if(ch=='a'||ch=='e'||ch=='i') c++; }` —
///    charAt + short-circuit `||` chain of char equality tests driving a conditional
///    increment in a loop).
///  - `LongCmp` (`if(a<b) return -1; if(a>b) return 1; return 0` over `long` params — the
///    wide-operand sibling of FloatCmp; the cmp result naturally lands in the free v0 below
///    the wide param pairs, so it was already correct, and stays byte-identical).
#[test]
fn stress_round22_byte_identical() {
    for name in ["CharLoop", "LongCmp"] {
        assert_byte_identical(name);
    }
}

/// 23rd semantic stress round, byte-identical — short-circuit booleans, intrinsics,
/// narrowing, and MORE multi-use float compares (re-validating the stress-22 cmp-liveness
/// fix on the `==`/`if-eqz` and ternary shapes):
///  - `AndChain` (`if(x>0 && x<10) …` — short-circuit `&&`: two `if-*z` with a shared
///    fall-through).
///  - `MathMax` (`Math.max(a, Math.max(b, c))` — nested `invoke-static` intrinsic).
///  - `FCmpChain` (`if(a<b) 1 else if(a==b) 0 else -1` over `float` — multi-use `a`,`b`
///    across `cmpg`/`cmpl` + `if-eqz`; the cmp-liveness fix keeps `a` live for the `==`).
///  - `NestTernF` (`a<b ? (b<c ? c : b) : a` — float nested ternary, three live params).
///  - `AbsLoop` (`for(i) s += Math.abs(a[i])` — `Math.abs` intrinsic call in a loop).
///  - `ShortNarrow` (`(short)(x+1)` — `int`→`short` `i2s` narrowing conversion).
#[test]
fn stress_round23_byte_identical() {
    for name in ["AndChain", "MathMax", "FCmpChain", "NestTernF", "AbsLoop", "ShortNarrow"] {
        assert_byte_identical(name);
    }
}

/// 71st semantic stress round — intrinsics / field arithmetic / single-use locals, byte-identical:
///  - `LastIdx` (`s.lastIndexOf('/')` — char-arg String virtual call → int).
///  - `FAbs` (`Math.abs(x)` over float — invoke-static).
///  - `IsLetter` (`Character.isLetter(c)` — char-arg invoke-static → boolean).
///  - `LongTernC2` (`b ? 1000000L : 0L` — ternary over two long constants).
///  - `DblFldAr` (`k * x` — double instance field times a double param (iget-wide + mul-double)).
///  - `MultiStmt` (`int p=a*b; int q=p+c; return q` — chained single-use locals).
///  - `ArrLenBr` (`int n=a.length; if(n>10) return n; return 0` — array-length to local then branch).
/// (FldArrSum2 `for(i){ int v=xs[i]; s+=v; }` over a field array — CORRECT getfield/cross-block-CSE
/// (d8 loads this.xs once per iter for both .length and [i]; skotch re-loads); same as FldArrSum;
/// deferred.)
#[test]
fn stress_round71_byte_identical() {
    for name in ["LastIdx", "FAbs", "IsLetter", "LongTernC2", "DblFldAr", "MultiStmt", "ArrLenBr"] {
        assert_byte_identical(name);
    }
}

/// `static final`/`static` float, double and String constant-field initializers are hoisted into
/// the class's `static_values` `encoded_array` byte-identically with d8. Previously skotch bailed
/// the whole static_values optimization whenever ANY static field was float/double, and dropped
/// `String` `ConstantValue`s — so a `static final double PI = 3.14159` field's value was silently
/// omitted from the DEX (it would read as the type default reflectively / at class init). The fix
/// adds `EncodedValue::Float`/`Double` (DEX VALUE_FLOAT 0x10 / VALUE_DOUBLE 0x11, right-zero-extended
/// encoding) and handles Float/Double/String in both the `ConstantValue` path and the pure-`<clinit>`
/// const-fold path. Covers:
///  - `DblConst` / `FloatConst` — `static final double`/`float` ConstantValue.
///  - `StrConst` — `static final String` ConstantValue (was dropped → bail).
///  - `StaticFinalD` — `static final double PI = 3.14159` inlined at every use (8-byte double value).
///  - `DblClinit` — non-final `static double X = 2.5; static double Y = 0.0` (clinit ldc2_w/dconst_0
///    fold + trailing-default trim of Y=0.0).
///  - `FloatClinit` — non-final `static float` (clinit ldc-float fold).
///  - `MixStat` — mixed `int` + `double` + `String` statics in one encoded_array.
#[test]
fn static_values_float_double_string_byte_identical() {
    for name in ["DblConst", "FloatConst", "StrConst", "StaticFinalD", "DblClinit", "FloatClinit", "MixStat"] {
        assert_byte_identical(name);
    }
}

/// 78th stress round — array-index + field shapes, 5 byte-identical wins. ALSO documents that d8's
/// `aget` result-register choice is ALLOCATOR-bound and inconsistent (probed AgetPP→array, AgetPC→array,
/// AgetTC→array, but AgetTP→INDEX): skotch's array-coalesce matches the param-array shapes but diverges
/// (same-size, correct) on temp-array shapes — a register-coalescing-choice divergence needing d8's
/// debug-aware linear-scan allocator, NOT cleanly fixable in the bootstrap path. Covers:
///  - `AgetPP` (`a[i]` array+index params), `AgetPC` (`a[0]` array param + const index).
///  - `FldSub` (`a - b` two fields), `FldNeg` (`-v` field neg), `StaticGet` (`COUNT + 1` static read).
/// (AgetTP/AgetTC/FldCmp DIVERGE same-size = correct allocator-bound register-coalescing choices.)
#[test]
fn stress_round78_byte_identical() {
    for name in ["AgetPP", "AgetPC", "FldSub", "FldNeg", "StaticGet"] {
        assert_byte_identical(name);
    }
}

/// getfield result COALESCES into a dead-temp receiver register (matching d8 `iget v0, v0`), like
/// `array-length`/`instance-of` already did — for chained `a.b.c` the intermediate temp is reused.
/// A PARAMETER receiver still gets a FRESH result (`iget v0, v1`, args-high kept). Covers:
///  - `ChainFld` (`next.next.v` — two chained iget-objects + iget; temp receivers coalesce).
///  - `NullTern` (`x != null ? x : y` — null-compare ternary over refs).
///  - `TernArg` (`Math.abs(b ? x : y)` — ternary as a method argument).
/// (FldArrIdx `arr[i]`, FldMeth `list.size()`, FldMul `a*b+a` DIVERGE but are CORRECT — register-
///  coalescing-choice [d8 reuses the index reg for aget; load-to-load for FldMul] — deferred.)
#[test]
fn getfield_coalesce_into_temp_receiver_byte_identical() {
    for name in ["ChainFld", "NullTern", "TernArg"] {
        assert_byte_identical(name);
    }
}

/// 77th semantic stress round — less-tested DEX encodings, a CLEAN 8/8 SWEEP:
///  - `LCmpBr` (`if(a>b)` over long — lcmp 0x31 feeding a branch).
///  - `UShift` (`x >>> n` — unsigned shift ushr-int by a variable).
///  - `DRem` (`a % b` over double — rem-double), `DCmpBr` (`a < b` over double — cmpg-double).
///  - `ArrLenA` (`a.length * 2` — array-length feeding arithmetic).
///  - `CharAri` (`(char)(c + 1)` — int-to-char on an expression).
///  - `NestTernRef` (`x>0 ? "pos" : (x<0 ? "neg" : "zero")` — nested ternary returning a ref).
///  - `BoolArr` (`a[i]` over boolean[] — aget-boolean, SSA-routed via the byte/bool array op).
#[test]
fn stress_round77_byte_identical() {
    for name in ["LCmpBr", "UShift", "DRem", "DCmpBr", "ArrLenA", "CharAri", "NestTernRef", "BoolArr"] {
        assert_byte_identical(name);
    }
}

/// STATIC-field store-to-load forwarding — the `sput`/`sget` parallel of the instance-field case,
/// reusing the same `FieldStore` machinery with an `obj_reg = STATIC_OBJ` sentinel. `s += x; return s`
/// forwards the `sput` value into the return instead of re-loading. Covers:
///  - `StatFwd` (`static int s; s += x; return s`).
///  - `StatNeg` (`s = x; side(); return s` — NEGATIVE: the call forces a re-load; byte-identical to d8
///    proves no forward across the call).
#[test]
fn static_field_store_to_load_forwarding_byte_identical() {
    for name in ["StatFwd", "StatNeg"] {
        assert_byte_identical(name);
    }
}

/// FIELD store-to-load forwarding: an `iput` immediately followed (nothing emitted between — only
/// no-op stack ops like `aload`/`dup`) by an `iget` of the SAME (object register, field) returns the
/// stored value register instead of re-loading, matching d8 (`f += x; return f` → `iget;add;iput;
/// return v0`, not a redundant `iget`). SAFE by construction: any intervening invoke / field write /
/// array store / branch emits an instruction, so `insns.len()` changes and the forward is suppressed
/// (no stale forward possible). Covers:
///  - `LongFldAdd` (`long t; t += x; return t` — wide field; was a +4-byte divergence before this).
///  - `IntFld` (`int t; t += x; return t` — narrow field).
///  - `NegFwd` (`t = x; side(); return t` — NEGATIVE: the intervening call forces a re-load; byte-
///    identical to d8 proves skotch does NOT forward across the call → never a stale-forward miscompile).
#[test]
fn field_store_to_load_forwarding_byte_identical() {
    for name in ["LongFldAdd", "IntFld", "NegFwd"] {
        assert_byte_identical(name);
    }
}

/// 76th semantic stress round — less-tested DEX encodings, 4 byte-identical wins:
///  - `ShiftVar` (`x << n` — shl-int by a variable register, not a const).
///  - `ByteCast` (`(byte)(x + 1)` — int-to-byte 0x8d on an expression result).
///  - `WideTern` (`b ? x : y` over long — ternary selecting a wide value).
///  - `CharSw` (`if(c=='a').. if(c=='b')..` — char-compare if-chain).
/// (Sync `synchronized` BAILS — monitor-enter 0xc2. Arr2D `new int[a][b]` BAILS — multianewarray 0xc5.
///  NewFill `new int[]{1,2,3}` BAILS — array-literal/filled-new-array. LongFld `t += x; return t` over a
///  long field is a CORRECT divergence (skotch +4 bytes): d8 store-to-load-forwards the iput-wide value
///  into the return; skotch re-loads via a redundant iget-wide. Same result; field store-to-load
///  forwarding is deferred — implementing it needs aliasing invalidation, miscompile risk.)
#[test]
fn stress_round76_byte_identical() {
    for name in ["ShiftVar", "ByteCast", "WideTern", "CharSw"] {
        assert_byte_identical(name);
    }
}

/// `instance-of` (0x20) is position-bearing for debug info, like `check-cast` — d8 records a line for
/// it. skotch recorded a position for check-cast but NOT instance-of, so `o instanceof String` dropped
/// its debug_info (param local + line) and came out 8 bytes short. Now fixed; covers:
///  - `InstOf` (`o instanceof String` — `instance-of v0,v0,String; return v0` + debug).
///  - `CmpChain` (`a > b && b > 0`), `CastObj` (`(String)o`), `StrEq` (`a.equals(b)`),
///    `ArrNew` (`new int[n]`), `Tern2` (`x > 0 ? x : -x`) — fresh shapes from the same round.
#[test]
fn instanceof_debug_position_byte_identical() {
    for name in ["InstOf", "CmpChain", "CastObj", "StrEq", "ArrNew", "Tern2"] {
        assert_byte_identical(name);
    }
}

/// 75th semantic stress round — fresh arithmetic/intrinsic shapes, a CLEAN 6/6 SWEEP:
///  - `BitAnd3` (`a & b & c` — chained and-int), `LongOr` (`a | b` over long).
///  - `NegD` (`-x` over double — neg-double), `AbsL` (`Math.abs(x)` over long).
///  - `MaxL` (`Math.max(a,b)` over long), `StrLen` (`s.length()`).
#[test]
fn stress_round75_byte_identical() {
    for name in ["BitAnd3", "LongOr", "NegD", "AbsL", "MaxL", "StrLen"] {
        assert_byte_identical(name);
    }
}

/// type_list section ORDER: d8 emits each class's interfaces type_list (from the class_def) BEFORE
/// the proto-param type_lists (collected later from method protos). skotch previously emitted protos
/// first — which only matched classes WITHOUT interfaces (e.g. ConvAll). Found by tracing a real
/// kotlin class (AbstractMutableList): d8 had `[List,KMutableList]` (interfaces) before `[I]`/`[I,
/// Object]` (protos). `IfaceImpl` (`class implements Iface` with `m(int)` and `n(int,Object)`)
/// exercises both a class-interface type_list and proto-param type_lists, byte-identical only with the
/// interfaces-first order.
#[test]
fn type_list_interfaces_before_protos_byte_identical() {
    assert_byte_identical("IfaceImpl");
}

/// InnerClass / Enclosing{Class,Method} / MemberClasses SYSTEM annotations (visibility 2), synthesized
/// from the classfile `InnerClasses` / `EnclosingMethod` attributes — pervasive in Kotlin (every
/// lambda → anonymous class, every nested class). Needs VALUE_TYPE (0x18 = type_idx) and VALUE_METHOD
/// (0x1a = method_idx) encoded values. Rules (decoded from d8): a class that appears as an `inner` in
/// some InnerClasses entry gets a `dalvik.annotation.InnerClass{accessFlags:int, name:String|null}`,
/// plus EnclosingMethod (local/anonymous, value=VALUE_METHOD) or EnclosingClass (member, value=
/// VALUE_TYPE); a class with nested members gets `MemberClasses{value:Type[]}`. Covers (filenames are
/// labels; classes are Outer / Outer$Inner / Outer$1, javac `-source 8` so they dex standalone):
///  - `NestHost` (`Outer` — declares a static nested class → MemberClasses[LOuter$Inner;]).
///  - `NestStatic` (`Outer$Inner` — static member → EnclosingClass(LOuter;) + InnerClass(8,"Inner")).
///  - `NestAnon` (`Outer$1` — anonymous → EnclosingMethod(Outer.make) + InnerClass(0, null name)).
#[test]
fn inner_class_system_annotations_byte_identical() {
    for name in ["NestHost", "NestStatic", "NestAnon"] {
        assert_byte_identical(name);
    }
}

/// ENUM annotation element values (`VALUE_ENUM` 0x1b = the enum constant's static field index) and
/// the empty-annotation-set rule. Enum values (e.g. `@Retention(RUNTIME)`, `@Target({TYPE,METHOD})`)
/// were previously unsupported, making skotch skip ALL annotations on the class — which blocked every
/// Kotlin `@interface` (they carry @Retention/@Target). ALSO: d8 emits the empty annotation_set
/// singleton ONLY when the dex has a class_data_item — a pure abstract marker annotation interface
/// (no methods/fields) omits it. Together these took whole-class byte-id on a 60-class kotlin sample
/// from 1 → 5. Covers:
///  - `EnumAnnVal` (`@A(p=RetentionPolicy.CLASS)` — one enum element → VALUE_ENUM field index).
///  - `EnumArrayAnnVal` (`@A(t={TYPE,METHOD})` — enum array → VALUE_ARRAY of VALUE_ENUM).
///  - `Target` — REAL `kotlin.annotation.Target` @interface (enum-array element + meta-annotations).
///  - `MustBeDocumented` / `Repeatable` — REAL Kotlin marker annotation interfaces with NO class_data,
///    so d8 (and now skotch) emits NO empty annotation_set singleton.
#[test]
fn enum_and_empty_set_annotations_byte_identical() {
    for name in ["EnumAnnVal", "EnumArrayAnnVal", "Target", "MustBeDocumented", "Repeatable"] {
        assert_byte_identical(name);
    }
}

/// FIELD- and METHOD-level annotations via the annotation_directory's `annotated_fields` /
/// `annotated_methods` arrays (each member with annotations gets its own annotation_set + items).
/// d8's layout order (cracked by tracing): items AND sets are laid out per class as methods (by
/// method_idx), then fields (by field_idx), then class; the empty-set singleton is first; the
/// directory lists field_annotation[] (by field_idx) then method_annotation[] (by method_idx); and
/// `class_annotations_off` points at the EMPTY set when only members are annotated (FieldSignature
/// has no class annotation yet class_off → the empty singleton). Covers:
///  - `FieldSignature` (`class { List<String> f; }` — one field Signature → annotated_fields=1,
///    class_off=empty set).
///  - `FieldMethodSignature` (`class FM<T> { List<String> f; <U> U pick(U); }` — class Signature +
///    field Signature + method Signature together; exercises the full methods→fields→class order).
/// (FM2 with 2 fields + 2 generic methods diverges only in the generic-method CODE — a pre-existing
///  register-numbering issue in `pick`/`wrap` bodies, NOT the annotation directory, which matches.)
#[test]
fn field_method_annotations_byte_identical() {
    for name in ["FieldSignature", "FieldMethodSignature"] {
        assert_byte_identical(name);
    }
}

/// Class-level generic `Signature` → `dalvik.annotation.Signature` SYSTEM annotation (visibility 2).
/// d8 synthesizes it from the classfile `Signature` attribute, with one element `value` = the
/// signature split into chunks: each `L…;`/`L…<` class type is its own chunk (the `<` is CONSUMED
/// into the chunk), the runs between/around them are chunks (dx's `splitSignature`). Generics are
/// pervasive in Kotlin, so this is a prerequisite for whole-class byte-identity. Covers:
///  - `GenericClass` (`class G<T extends Number>` — `<T:Ljava/lang/Number;>Ljava/lang/Object;` →
///    `["<T:", "Ljava/lang/Number;", ">", "Ljava/lang/Object;"]`).
///  - `GenericSuper` (`class GL extends ArrayList<String>` — `Ljava/util/ArrayList<Ljava/lang/
///    String;>;` → `["Ljava/util/ArrayList<", "Ljava/lang/String;", ">;"]`; exercises `<`-consumption).
/// (Field/method-level Signatures need annotated_fields/methods in the annotation_directory — next.)
#[test]
fn class_signature_annotation_byte_identical() {
    for name in ["GenericClass", "GenericSuper"] {
        assert_byte_identical(name);
    }
}

/// ELEMENT-BEARING class annotations: the `encoded_annotation` carries `uleb(name_idx) encoded_value`
/// pairs (sorted by element-name string index, as d8 requires); array element values use VALUE_ARRAY
/// (0x1c). This unlocks @kotlin.Metadata, the per-class annotation that made whole-class byte-identity
/// ~0% on real code. Covers (filenames are labels; embedded class names CI/CS/CA/CM):
///  - `AnnInt` (`@A(v=5)` — one int element, VALUE_INT).
///  - `AnnStr` (`@A(s="hello")` — one String element, VALUE_STRING).
///  - `AnnArr` (`@A(iv={1,2,3}, sv={"a","b"})` — int[] and String[] elements, nested VALUE_ARRAY).
///  - `AnnMulti` (`@A(k=2, name="x", arr={5,6})` — three elements, exercises name-string-idx sort).
///  - `CharCodeJVMKt` — a REAL kotlin-stdlib class carrying `@kotlin.Metadata` (int/int[]/String[]
///    elements); the FIRST real-world whole-class byte-identical result (was a divergence before
///    element-bearing annotations landed).
#[test]
fn element_annotations_byte_identical() {
    for name in ["AnnInt", "AnnStr", "AnnArr", "AnnMulti", "CharCodeJVMKt"] {
        assert_byte_identical(name);
    }
}

/// Class-level RUNTIME annotation emission (the foundation for whole-class byte-identity, which is
/// ~0% on real code because every Kotlin class carries @Metadata). Emits annotation_item (0x2004,
/// before class_data), the per-class annotation_set (0x1003, after the empty singleton), and
/// annotation_directory_item (0x2006, after the sets), wiring class_def.annotations_off. Rules
/// matched to d8: only RUNTIME-retention (visibility 1) user annotations survive (CLASS-retention is
/// dropped); annotation_items are emitted SORTED by (type_idx, visibility); set entries sorted by
/// type_idx. Covers (filenames are labels; the embedded class names are C/D/E/G):
///  - `AnnMarker` (`@Ann class` — one no-element RUNTIME annotation).
///  - `AnnTwo` (`@Ann @Ann2 class` — two markers; exercises type_idx-sorted item + set order).
///  - `AnnDeprecated` (`@Deprecated class` — a JDK RUNTIME annotation).
///  - `AnnClassRetention` (`@AnnB class` where AnnB is CLASS-retention — d8 emits NO annotation, so
///    skotch must drop it too; guards against over-emitting RuntimeInvisible annotations).
/// (Element-bearing annotations like @kotlin.Metadata are NOT yet emitted — a class with one is left
/// without its annotation directory, a divergence that still dexes fine. That's the next step.)
#[test]
fn class_annotations_byte_identical() {
    for name in ["AnnMarker", "AnnTwo", "AnnDeprecated", "AnnClassRetention"] {
        assert_byte_identical(name);
    }
}

/// A discarded method-call result (`invoke; pop`/`pop2`) emits NO `move-result` — matching d8.
/// `nop` (0x00) is dropped (not in d8's IR). Measurement-driven: pop/nop was the #5/#6 bail bucket
/// on real kotlin-stdlib (~25 classes); this raised whole-class dex coverage 49.2% → 50.4%. The
/// invoke handler peeks the next opcode and, when a non-void result is immediately discarded by the
/// matching `pop` (narrow) / `pop2` (wide), suppresses the result-register allocation entirely — so
/// `registers_size` stays correct (`void f(){ System.currentTimeMillis(); }` is 0 registers in d8,
/// not 2). Covers:
///  - `DiscardCall` (`sb.append("x")` — discarded virtual call returning a ref).
///  - `DiscardStatic` (`Integer.parseInt(s)` — discarded static returning int → pop).
///  - `DiscardWide` (`System.currentTimeMillis()` — discarded static returning long → pop2; 0 regs).
/// (A standalone `pop` of a non-invoke value — e.g. `getfield; pop` — still BAILS: its producing
/// instruction has observable side effects we can't safely rewind. nop has no javac fixture but is
/// exercised by the kotlin-stdlib coverage corpus.)
#[test]
fn discard_result_byte_identical() {
    for name in ["DiscardCall", "DiscardStatic", "DiscardWide"] {
        assert_byte_identical(name);
    }
}

/// `athrow` (JVM 0xbf) → DEX `throw vAA` (0x27, 11x) in the straight-line and CFG bootstrap paths
/// (the SSA path already handled it for try/catch). d8 treats `throw` as position-bearing, so both
/// paths record a line at the throw. Measurement-driven: athrow was the #3 bail bucket on real
/// kotlin-stdlib (40 classes), and adding it raised whole-class dex coverage 45.9% → 49.2% (+33
/// classes). Covers:
///  - `Rethrow` (`throw e` — rethrow a param ref: `aload; athrow` → `throw v0`).
///  - `ThrowNew` (`throw new RuntimeException()` — new-instance + invoke-direct + throw).
///  - `ThrowMsg` (`throw new IllegalStateException(s)` — new + ctor-with-arg + throw).
/// (CondThrow `if(x<0) throw e` is a CORRECT branch-inversion/block-reorder divergence — d8 flips
///  `ifge`→`if-ltz` and moves the non-falling-through throw block to the end; same size, same
///  semantics. ThrowField `throw this.t` with a `throws Throwable` clause diverges on the ANNOTATION
///  gap — d8 emits `dalvik.annotation.Throws`; skotch emits no annotations yet. Both deferred.)
#[test]
fn athrow_byte_identical() {
    for name in ["Rethrow", "ThrowNew", "ThrowMsg"] {
        assert_byte_identical(name);
    }
}

/// WIDENING numeric conversions (i2l/i2d/l2d/f2l/f2d) in an otherwise straight-line method are now
/// routed to the SSA path (which models wide conversions, #22) and emit byte-identically, instead of
/// bailing in the straight-line path. A widening conv's wide result needs args-high placement (e.g.
/// `(double)x` → `int-to-double v0, v2`), which only the SSA allocator reproduces. Covers:
///  - `I2Donly`/`I2Lonly`/`L2Donly`/`F2Lonly`/`F2Donly` (`(double)x`,`(long)x`,… single conversion).
///  - `CastChain` (`(double)(x+y)` — add then i2d; previously a bail in stress_round74).
///  - `SumLong` (`(long)a + b`), `MulL` (`(long)a * (long)b`) — i2l feeding long arithmetic.
///  - `SqrtI` (`Math.sqrt((double)x)` — i2d result feeds a method ARG, NOT a double-arith op, so d8
///    inserts no isNaN workaround → byte-identical).
///  - `DblCmpConv` (`(double)x > y` — i2d feeding a dcmp+branch; routed via the branch, no isNaN).
#[test]
fn widening_conversions_byte_identical() {
    for name in ["I2Donly", "I2Lonly", "L2Donly", "F2Lonly", "F2Donly", "CastChain", "SumLong", "MulL", "SqrtI", "DblCmpConv"] {
        assert_byte_identical(name);
    }
}

/// Two widening-conversion shapes DELIBERATELY stay on the bail path (a loud bail beats a silent
/// CORRECT-but-divergent output):
///  - `AvgD` (`(a+b)/2.0`): a to-DOUBLE conversion result feeding a double-ARITH op triggers d8's
///    min-api ART-bug WORKAROUND — a discarded `Double.isNaN(convResult)` call emitted before the op
///    (present even without `-g`). That's a desugaring-class artifact (Phase 2); skotch's faithful
///    translation omits it, so route such methods to the bail rather than diverge. The detector
///    `method_has_widening_conv` excludes any method with both a to-double conv and a double op.
///  - `L2Ionly` (`(int)x` from a long): d8 places the narrow result in the source long's HIGH
///    register (`long-to-int v1, v0`); the SSA allocator picks v0 — a CORRECT same-size divergence,
///    so l2i (0x88) is not a routing trigger.
/// `AvgD` (double averaging — d8 emits a desugaring-class artifact skotch omits) and
/// `L2Ionly` (`(int)x` from a long — d8 places the narrow result in the source's HIGH
/// register; the SSA allocator picks differently). Both were BAILED to avoid byte-id
/// divergence; now in functional-correctness mode they DEX via the SSA fallback. NOT
/// byte-identical to d8 — correctness proven by `tests/art/ArtFallback` (double
/// division + l2i exercised on ART). Here: dexes + self-validates.
#[test]
fn widening_conv_and_l2i_now_dex_via_ssa_fallback() {
    for name in ["AvgD", "L2Ionly"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// 74th semantic stress round — HARDER shapes (arithmetic widths, nested control flow, bitwise
/// chains), 8 byte-identical wins:
///  - `DblMod` (`a % b` over double — rem-double).
///  - `LongDiv2` / `LongRem` (`a / b` / `a % b` over long — div-long / rem-long).
///  - `ArrSum3` (`a[0]+a[1]+a[2]` — three array-element loads chained).
///  - `TernNest` (`a>b ? (c>d?1:2) : 3` — NESTED ternary, multi-block control flow).
///  - `ShiftOr` (`(x<<8) | y` — shl-int + or-int).
///  - `XorChain` (`a ^ b ^ c` — chained xor-int).
///  - `AndOr` (`(a & b) | c` — and-int then or-int).
/// (CastChain `(double)(x+y)` bailed here as i2d 0x87 — now byte-identical via the widening-conv SSA
///  routing, see widening_conversions_byte_identical. CharRange2 `c>='a'&&c<='z'` over a char PARAM
///  was byte-identical — distinct from the array-element char-range which diverges via cross-block
///  CSE, because a param needs no re-load.)
#[test]
fn stress_round74_byte_identical() {
    for name in ["DblMod", "LongDiv2", "LongRem", "ArrSum3", "TernNest", "ShiftOr", "XorChain", "AndOr"] {
        assert_byte_identical(name);
    }
}

/// 73rd semantic stress round — a CLEAN 8/8 SWEEP of single-use intrinsic shapes, byte-identical
/// (no divergences, no bails):
///  - `CompareStr` (`a.compareTo(b)` — String.compareTo → int).
///  - `BitCnt` / `LBitCnt` (`Integer.bitCount(x)` / `Long.bitCount(x)` — popcount, int return).
///  - `Contains` (`s.contains(t)` — String.contains(CharSequence) → boolean).
///  - `RevInt` (`Integer.reverse(x)` — bit-reverse).
///  - `TrailZ` (`Long.numberOfTrailingZeros(x)` — long-arg → int).
///  - `SignI` (`Integer.signum(x)`).
///  - `Expm1` (`Math.expm1(x)` — double intrinsic; NOT desugared, unlike Math.*Exact).
#[test]
fn stress_round73_byte_identical() {
    for name in ["CompareStr", "BitCnt", "LBitCnt", "Contains", "RevInt", "TrailZ", "SignI", "Expm1"] {
        assert_byte_identical(name);
    }
}

/// 72nd semantic stress round — single-use / intrinsic / field-array shapes, byte-identical:
///  - `IsUpper2` (`Character.isUpperCase(c)` — char-arg invoke-static → boolean).
///  - `Hypot2` (`Math.sqrt(a*a + b*b)` — double arithmetic into an intrinsic).
///  - `LongFldAr` (`total + x*2L` — long instance field plus long-const multiply).
///  - `CharWiden2` (`char a + b` → int — char widening add).
///  - `TwoArrAdd` (`a[i] + b[i]` — two array params, distinct array registers).
///  - `SubInt` (`s.substring(i, i+3)` — two-arg String.substring).
#[test]
fn stress_round72_byte_identical() {
    for name in ["IsUpper2", "Hypot2", "LongFldAr", "CharWiden2", "TwoArrAdd", "SubInt"] {
        assert_byte_identical(name);
    }
}

/// 70th semantic stress round — intrinsics / field-array / loop, byte-identical:
///  - `IndexOfFrom` (`s.indexOf('x', 2)` — two-arg String.indexOf(char, fromIndex)).
///  - `ToLower` (`Character.toLowerCase(c)` — char→char intrinsic).
///  - `FldArrW` (`buf[i] = v` — store into an instance-field array (iget-object + aput)).
///  - `LoopXor` (`for(i) x ^= a[i]` — XOR accumulator loop).
/// (BranchMul `if(a>0) return a*b` BAILS — imul 0x68 in CFG path. LongRet `(long)a*b` BAILS — i2l
/// 0x85 widening. NextDown `Math.nextDown(x)` — DESUGARING (d8 synthetic backport, API-26).
/// StrCharCmp `s.charAt(0)=='a'` — const-CSE (d8 reuses index const 0 as boolean false). Deferred.)
#[test]
fn stress_round70_byte_identical() {
    for name in ["IndexOfFrom", "ToLower", "FldArrW", "LoopXor"] {
        assert_byte_identical(name);
    }
}

/// 69th semantic stress round — single-use / clean shapes, byte-identical:
///  - `FldArr` (`a[i] + offset` — array load + instance field add).
///  - `SingleUseLoop` (`for(i){ int v = a[i]*2; s += v; }` — single-use local in a loop body).
///  - `DblSignum` (`Math.signum(x)` over double — invoke-static).
///  - `LongShAdd` (`(x << n) + x` over long — shl-long + add-long, x used twice but single-assign
///    of no local — the shift and add both read the param directly, no multi-use local).
///  - `ToHex` (`Integer.toHexString(x)` — invoke-static returning a String).
/// (CharBr `if(c>='0'&&c<='9') return c-'0'` BAILS — isub 0x64 in CFG path. DblTernAr `a>b?a-b:b-a`
///  over double — CORRECT ternary reg-unification (skotch 3-addr to unify branches, d8 2-addr).
///  ElemParam `a[0]==x` — CORRECT const-CSE (d8 reuses index const 0 as the boolean false). Deferred.)
#[test]
fn stress_round69_byte_identical() {
    for name in ["FldArr", "SingleUseLoop", "DblSignum", "LongShAdd", "ToHex"] {
        assert_byte_identical(name);
    }
}

/// MULTI-USE single-assignment locals in the straight-line path now DEX (functional-correctness mode):
/// each gets a PINNED stable register that survives every read and resists coalescing (so `int v=a[i];
/// return v*v + v` keeps `v` live across the `v*v` — no clobber). Validated functionally by ART
/// execution (cf. the `art_exec` ArtLocals fixture: triple `a+a+a`, two multi-use, wide, const-multi —
/// all run correctly on ART). NOT necessarily byte-identical to d8's allocator. Here we just assert
/// they DEX and self-validate (previously they bailed "single-assignment single-use only").
#[test]
fn multi_use_local_straightline_now_dexes() {
    for name in ["AbsLocal", "ElemTwice"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: multi-use local should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// 68th semantic stress round — single-use-local / char-range shapes, byte-identical:
///  - `IsAlpha` (`(c>='A'&&c<='Z') || (c>='a'&&c<='z')` — nested char-range &&/|| → boolean).
///  - `DblTmp` (`double s=a+b; double d=a-b; return s*d` — two single-use double temps).
///  - `SelStore` (`int m = a>b ? a : b; return m+1` — ternary stored to a local then used once).
///  - `ThreeTmp` (`int x=a+b; int y=b+c; int z=a+c; return x*y*z` — three single-use temps).
/// (AbsLocal/FldLocal/ElemTwice/LongLocal BAIL — a local read TWICE (`int v=…; v*v`) is a
/// multi-use local, which the straight-line path doesn't allocate (register-pressure bail,
/// single-assignment single-use only). The same multi-use local in a LOOP works via the SSA path
/// (cf. TmpReuse) — only the no-loop straight-line path bails. Safe, by design.)
#[test]
fn stress_round68_byte_identical() {
    for name in ["IsAlpha", "DblTmp", "SelStore", "ThreeTmp"] {
        assert_byte_identical(name);
    }
}

/// 67th semantic stress round — local-temp / intrinsic shapes, byte-identical:
///  - `IsWS` (`Character.isWhitespace(c)` — char-arg invoke-static → boolean).
///  - `LNLZ` (`Long.numberOfLeadingZeros(x)` — wide-arg invoke-static → int).
///  - `TwoTmp` (`int x=a+b; int y=a-b; return x*y` — two local temps feeding a multiply).
///  - `BrTmp` (`int d=a-b; if(d>0) return d; return -d` — abs via a local temp + branch).
///  - `FldParam2` (`base + mult * x` — two instance fields + a param).
///  - `BitCombo` (`Integer.bitCount(a) + Long.bitCount(b)` — two distinct bit-count intrinsics).
///  - `ThreeSum` (`int x=a[0]; int y=a[1]; int z=a[2]; return x+y+z` — three local temps).
/// (ManualMod `int r=a%b; return r<0 ? r+b : r` — CORRECT return-tail-merge divergence (d8 merges
/// both paths to one `return v0`; skotch emits both); same total size; deferred.)
#[test]
fn stress_round67_byte_identical() {
    for name in ["IsWS", "LNLZ", "TwoTmp", "BrTmp", "FldParam2", "BitCombo", "ThreeSum"] {
        assert_byte_identical(name);
    }
}

/// 66th semantic stress round — LOOP idioms (parse/hash/running-max), byte-identical:
///  - `DigitParse` (`for(i) n = n*10 + (s.charAt(i) - '0')` — parseInt-style digit accumulation).
///  - `RunMax` (`m=a[0]; for(i){ int v=a[i]; if(v>m) m=v; }` — running max via local temp; the
///    local temp avoids the cross-block reload that the inline ArrMax/MinLoop versions hit).
///  - `HashLoop` (`for(i) h = 31*h + s.charAt(i)` — String.hashCode-style accumulation).
///  - `SumPairs` (`for(i){ int sq=i*i; s += sq + sq; }` — local temp reused in arithmetic).
/// (AllPos `boolean ok=true; for if(a[i]<=0) ok=false` — CORRECT d8-dead-const-quirk (d8 emits a
/// spurious unused `const 0`; skotch is tighter). FldArrSum `for(i) s+=data[i]` (field array) —
/// CORRECT getfield/cross-block-CSE (d8 loads this.data once per iter; skotch re-loads). SumCast
/// `long s; for s+=a[i]; return (int)s` — CORRECT long-to-int result reg-reuse. FirstNeg
/// `for{int v=a[i]; if(v<0) return v;} return 0` — CORRECT const-hoist of the return-0. All deferred.)
#[test]
fn stress_round66_loops_byte_identical() {
    for name in ["DigitParse", "RunMax", "HashLoop", "SumPairs"] {
        assert_byte_identical(name);
    }
}

/// 65th semantic stress round — LOOP shapes (local-temp reuse avoids cross-block-CSE), byte-identical:
///  - `TmpReuse` (`for(i){ int t=a[i]; s += t*t; }` — local temp reused twice; no cross-block reload).
///  - `TwoCondWhile` (`while(n>0 && m>0){ n--; m--; c++; }` — two-condition while loop).
///  - `FldProd` (`for(i) p *= factor` — multiply by an instance field in a loop).
///  - `LoopArith` (`for(i) s += i*i + i` — nested arithmetic loop body, no array re-read).
///  - `Transform` (`for(i) out[i] = in[i] + 1` — read one array, write another).
///  - `CountPos` (`for(i){ int v=a[i]; if(v>0) c+=v; }` — local temp guards the cross-block reload).
///  - `RunMul` (`long r=1; for(i) r = r*i` — wide running product).
/// (DoAccum `do{ s += i*2; i++; } while(i<n)` — CORRECT loop-variable register-numbering swap
/// (d8 s=v0/i=v1, skotch i=v0/s=v1); same instructions, same size; deferred.)
#[test]
fn stress_round65_loops_byte_identical() {
    for name in ["TmpReuse", "TwoCondWhile", "FldProd", "LoopArith", "Transform", "CountPos", "RunMul"] {
        assert_byte_identical(name);
    }
}

/// 64th semantic stress round — LOOP shapes incl. array-write-in-loop, byte-identical:
///  - `ArrFill` (`for(i) a[i] = i*i` — array element WRITE in a loop; the historically-risky shape
///    where the array-literal & float-cmp miscompiles were originally found — here correct).
///  - `TwoAccum` (`for(i){ sum+=a[i]; cnt++; }` — two accumulators updated per iteration).
///  - `GeoLoop` (`for(i) x *= 2` — geometric/doubling accumulator).
///  - `AdjCmp` (`for(i=1) if(a[i] > a[i-1]) c++` — adjacent-element comparison).
///  - `CharSum2` (`for(i) sum += s.charAt(i)` — String.charAt accumulation loop).
///  - `CallAccum` (`for(i) s += g(i)` — method-call body accumulating).
/// (NestIfLoop `for if(a[i]>0) if(a[i]<100) s+=a[i]` and SumEven `for if(a[i]%2==0) s+=a[i]` —
/// CORRECT cross-block redundant-load-CSE divergences: d8 loads a[i] once and reuses across blocks;
/// skotch's block-local CSE re-loads it (SumEven additionally clobbers a[i] with the rem result,
/// forcing the re-load, where d8 uses a fresh rem dest). Traced correct, larger; deferred.)
#[test]
fn stress_round64_loops_byte_identical() {
    for name in ["ArrFill", "TwoAccum", "GeoLoop", "AdjCmp", "CharSum2", "CallAccum"] {
        assert_byte_identical(name);
    }
}

/// 63rd semantic stress round — more LOOP shapes, byte-identical:
///  - `FldAccum` (`for(i) total += a[i]` on an instance field — field RMW across iterations).
///  - `LongSum` (`long s=0; for(i) s += a[i]` over `long[]` — aget-wide + add-long accumulator).
///  - `DblSum` (`double s=0; for(i) s += a[i]` over `double[]` — aget-wide + add-double).
///  - `DotLoop` (`for(i) s += a[i] * b[i]` — dot product, two arrays).
///  - `CountMatch` (`for(i) if(a[i]==x) c++` — count matching elements).
/// (MinLoop `m=a[0];for if(a[i]<m)m=a[i]` and BreakLoop `for{if(a[i]==0)break; s+=a[i];}` — CORRECT
/// cross-block redundant-load-CSE (d8 reuses the compared a[i], skotch re-loads). RevRead
/// `for(i=len-1;i>=0;i--) s+=a[i]` — CORRECT const-placement/φ-init-order (i/s register assignment +
/// len-1-vs-const-0 init order swapped; same size, EnhFor class). All deferred.)
#[test]
fn stress_round63_loops_byte_identical() {
    for name in ["FldAccum", "LongSum", "DblSum", "DotLoop", "CountMatch"] {
        assert_byte_identical(name);
    }
}

/// 62nd semantic stress round — LOOP VARIETY focus, byte-identical (validates the SSA loop path):
///  - `DownLoop` (`for(i=n; i>0; i--) s+=i` — downward-counting loop).
///  - `StepLoop` (`for(i=0; i<n; i+=3) s+=i` — non-unit stride).
///  - `WhileDec` (`while(n>1){ n=n/2; c++; }` — log-style halving loop).
///  - `ProdLoop` (`for(i=1; i<=n; i++) p*=i` — product accumulator (factorial)).
///  - `LongAccum` (`long s=0; for(i) s+=i` — wide accumulator with int index).
///  - `FindLoop` (`for(i) if(a[i]==x) return i; return -1` — linear search, early return).
/// (ArrMax `m=a[0]; for(i) if(a[i]>m) m=a[i]` and ContLoop `for(i){ if(a[i]<0) continue; s+=a[i]; }`
/// — both CORRECT cross-block redundant-load-CSE divergences: d8 reuses the `a[i]` loaded for the
/// comparison (via move / reuse) in the following block; skotch's block-local CSE re-loads `a[i]`
/// with a fresh aget. Traced fully correct, skotch one unit larger; deferred (cross-block CSE).)
#[test]
fn stress_round62_loops_byte_identical() {
    for name in ["DownLoop", "StepLoop", "WhileDec", "ProdLoop", "LongAccum", "FindLoop"] {
        assert_byte_identical(name);
    }
}

/// 61st semantic stress round — EXCEPTION HANDLING focus, byte-identical (validates the SSA
/// try/catch path across fresh shapes):
///  - `TryRet` (`try { return a[i]; } catch(AIOOBE) { return -1; }` — return from try AND catch).
///  - `TryCount` (`try { c = a[i]; } catch(Exception) { c = 0; } return c` — catch writes a local).
///  - `DivCatch` (`try { return a/b; } catch(ArithmeticException) { return 0; }` — div in a try).
/// (Confirmed SAFE BAILS for unsupported exception shapes — zero miscompiles: ThrowEx/ThrowChk
/// (`throw new X()` — new-instance 0xbb in CFG path), NestTry (nested try regions), MultiCatch
/// (handler with >1 try-predecessor / `A|B` multi-catch), TryFinally (finally / catch-all). Each
/// bails with a clear feature-gap message rather than miscompiling.)
#[test]
fn stress_round61_exceptions_byte_identical() {
    for name in ["TryRet", "TryCount", "DivCatch"] {
        assert_byte_identical(name);
    }
}

/// 60th semantic stress round (edge shapes), byte-identical:
///  - `LongLtBool` (`a < b` over long → boolean directly (cmp-long + if-*z to materialize)).
///  - `DblConstAr` (`x + 3.14` — double-constant arithmetic via const-wide/high16... actually
///    const-wide + add-double).
///  - `MultiCharRet` (`if(x==0)'z'; if(x==1)'o'; return 't'` — char if-chain dispatch).
///  - `VoidSide` (`buf[i] = v` to a static array field — sget-object + aput, returns void).
///  - `FldShiftR` (`bits >> 2` — arithmetic shift of an instance field).
///  - `DblToFloat` (`(float)(x * 2.0)` — double arithmetic then double→float narrowing (d2f)).
/// (FloatFld `scale*x + 1.0f` — CORRECT const-placement/reg-reuse (the 1.0f goes into a different
/// dead register: d8 x's, skotch this's; same size). ArrBoolCmp `a[0] > 0` — CORRECT const-CSE
/// (d8 reuses index const 0 as the boolean false return; skotch re-materializes; larger). Deferred.)
#[test]
fn stress_round60_byte_identical() {
    for name in ["LongLtBool", "DblConstAr", "MultiCharRet", "VoidSide", "FldShiftR", "DblToFloat"] {
        assert_byte_identical(name);
    }
}

/// 59th semantic stress round (edge shapes), byte-identical:
///  - `BoolFld` (`return flag` — boolean instance field read).
///  - `LongArrLen` (`a.length` over `long[]` — array-length of a wide-element array).
///  - `CastCast` (`(byte)(short) x` — consecutive int→short→byte narrowing (i2s then i2b)).
///  - `BoolArrR` (`a[0]` over `boolean[]` — aget-boolean).
///  - `CondLong` (`c ? a+b : a-b` over long — ternary selecting between two long expressions).
///  - `CharRetCmp` (`a > b ? a : b` over char — char max via ternary).
///  - `NegCond` (`x < 0 ? -x : x*2` — ternary with distinct arithmetic on each branch).
/// (TernFld `b ? x : y` over two int fields — CORRECT getfield reg-reuse divergence: skotch reuses
/// dead `this` reg (`iget v0,v0`), d8 reuses dead `b` reg (`iget v1,v0`); both dead, same size.)
#[test]
fn stress_round59_byte_identical() {
    for name in ["BoolFld", "LongArrLen", "CastCast", "BoolArrR", "CondLong", "CharRetCmp", "NegCond"] {
        assert_byte_identical(name);
    }
}

/// 58th semantic stress round (edge shapes), byte-identical:
///  - `CharFld` (`c + s + b` over char/short/byte instance fields — iget-char/short/byte mix).
///  - `MixedCmp` (`x > c` with int x, char c — mixed-width comparison).
///  - `TernCallL` (`x>0L ? g(x) : h(x)` — long ternary selecting between two wide-arg calls).
///  - `LongChain2` (`if(a<b)-1; if(a==b)0; return 1` over long — two cmp-long + branches).
///  - `ShortFldW` (`this.v = (short) x` — int→short narrowing into a short field (iput-short)).
/// (ArrInit3 `new int[3]; a[0]=x;a[1]=x;a[2]=x` BAILS — multi-use local (1 store/4 loads) needs
/// the full allocator. MultiShift `(x<<24)|(x<<16)|(x<<8)|x` — CORRECT commutative-or operand-order.
/// StaticArr `data[i]` (static array field) — CORRECT array-load reg-reuse (d8 index reg, skotch
/// array reg; same as ArrFld). Both same size; deferred.)
#[test]
fn stress_round58_byte_identical() {
    for name in ["CharFld", "MixedCmp", "TernCallL", "LongChain2", "ShortFldW"] {
        assert_byte_identical(name);
    }
}

/// 57th semantic stress round, byte-identical — a clean sweep (8/8, no divergences/bails):
///  - `DblCmpC` (`if(x > 1.0) return 1; return 0` — double compare to a const driving a branch).
///  - `LongXorC` (`x ^ 0xffL` — long XOR with a constant).
///  - `ArrNeg` (`-a[0]` — array load feeding a negate).
///  - `FldDiv` (`x / divisor` — divide by an instance-field value).
///  - `ByteTern` (`x > 0 ? (byte)1 : (byte)-1` — ternary over two byte constants).
///  - `NestedRet` (`g(a) + g(b)` — two calls to the same static fn, results added).
///  - `CharSwitch3` (`if(c=='a')1; if(c=='b')2; return 0` — char if-chain dispatch).
///  - `LongAndC` (`x & 0xffffffL` — long AND with a constant mask).
#[test]
fn stress_round57_byte_identical() {
    for name in ["DblCmpC", "LongXorC", "ArrNeg", "FldDiv", "ByteTern", "NestedRet", "CharSwitch3", "LongAndC"] {
        assert_byte_identical(name);
    }
}

/// 56th semantic stress round, byte-identical — long/double cmp+cast, multi-arg call, computed index:
///  - `LongCmpC` (`if(x > 100L) return 1; return 0` — long compare to a const driving a branch).
///  - `DblToInt` (`(int)(x * 2.0)` — double arithmetic then double→int narrowing (d2i)).
///  - `ThreeArg` (`g(x, x+1, x+2)` — three-arg static call with computed args).
///  - `ArrIdxStore` (`a[i + 1] = v` — array store to a computed index).
///  - `LongShlC` (`x << 10` over long — shl-long by const).
///  - `FloatDiv` (`a / b` over float — div-float).
///  - `CharCond` (`if(c == '\n') return 1; return 0` — char equality branch).
/// (LongToDbl `x * 1.5` (long→double) BAILS — l2d 0x8a, deliberately bailed widening conversion.)
#[test]
fn stress_round56_byte_identical() {
    for name in ["LongCmpC", "DblToInt", "ThreeArg", "ArrIdxStore", "LongShlC", "FloatDiv", "CharCond"] {
        assert_byte_identical(name);
    }
}

/// 55th semantic stress round, byte-identical — double divide, char arith, field/user-call, ternary-add:
///  - `DblDiv` (`a / b` over double — div-double).
///  - `UpperArith` (`(char)(c - 32)` — char arithmetic with i2c).
///  - `FldMulParam` (`scale * x` — instance field times a param).
///  - `UserLong` (`g(x,y) * 2L` — user wide-arg static call result times a long const).
///  - `FloatNegMul` (`-a * b` over float — neg-float feeding mul-float).
///  - `CondAdd` (`a + (c ? b : 0)` — ternary value in an additive context).
/// (LongBitwise `(a&b)|(a^c)` — CORRECT commutative-`or` operand-order (d8 returns v0, skotch v2;
/// same size). ArrCmp `a[0]==a[1]` — CORRECT const-CSE divergence: d8 reuses the index constants
/// 0/1 directly as the boolean 0/1 return values; skotch re-materializes the booleans; larger.
/// Both deferred.)
#[test]
fn stress_round55_byte_identical() {
    for name in ["DblDiv", "UpperArith", "FldMulParam", "UserLong", "FloatNegMul", "CondAdd"] {
        assert_byte_identical(name);
    }
}

/// 54th semantic stress round, byte-identical — long/double arithmetic, field shift, mid-index:
///  - `LongDivC` (`x / 1000L` — long division by a non-power-of-two const → div-long).
///  - `DblRem` (`a % b` over double — rem-double).
///  - `CharCmpI` (`a<b ? -1 : (a>b ? 1 : 0)` over char — nested three-way char compare ternary).
///  - `Shadow` (`this.x + x` — param shadowing a field, both read and added).
///  - `ShiftFld` (`x << amount` — shift by an instance-field value (iget feeds shl-int)).
///  - `MidIdx` (`a[a.length / 2]` — array-length feeds an index computation then aget).
///  - `LongSar` (`x >> 3` over long — arithmetic shift-right by const → shr-long/lit8).
/// (CondNegD `neg ? -x : x` over double — CORRECT return-tail-merge divergence (double version of
/// CondNeg): d8 merges both paths to one `return-wide v0`; skotch emits both. Deferred.)
#[test]
fn stress_round54_byte_identical() {
    for name in ["LongDivC", "DblRem", "CharCmpI", "Shadow", "ShiftFld", "MidIdx", "LongSar"] {
        assert_byte_identical(name);
    }
}

/// 53rd semantic stress round, byte-identical — ternaries, array store, char, double cmp, statics:
///  - `LongTernC` (`x > 0 ? 100L : -100L` — ternary over two long constants).
///  - `ArrSet` (`a[0] = v` — array store from a param, straight-line).
///  - `CharRet` (`(char)(c + 1)` — char arithmetic with i2c, returning char).
///  - `DblLt` (`if(a<b) return 1; return 0` over double — cmpl-double + if branch).
///  - `ThreeStatic` (`a + b + c` over three static fields — three sget + two add).
///  - `NestTern2` (`a>0 ? (b>0?3:2) : (b>0?1:0)` — nested ternary over two vars, 4 outcomes).
/// (IntCast `(int)(x & 0xffffffffL)` BAILS — l2i 0x88. OrVal `(a>0||b>0)?1:0` — CORRECT
/// shared-exit fallthrough divergence (like OrEarly): skotch merges both true-paths into one
/// `return 1` block, dropping d8's explicit goto; skotch tighter; deferred.)
#[test]
fn stress_round53_byte_identical() {
    for name in ["LongTernC", "ArrSet", "CharRet", "DblLt", "ThreeStatic", "NestTern2"] {
        assert_byte_identical(name);
    }
}

/// 52nd semantic stress round, byte-identical — bitwise-not, static-final, multi-branch:
///  - `BitNot` (`~x & 0xff` — int bitwise-not (not-int) then mask).
///  - `LongNot` (`~x` over long — not-long).
///  - `StaticFinal` (`x * K` with `static final int K = 7` — sget of a final static constant).
///  - `FloatChain` (`a*b + c*d` over float — two mul-float + add-float).
///  - `LongArrAr` (`a[0]*a[1] + a[2]` over `long[]` — aget-wide + mul-long + add-long).
///  - `MaxIf` (`if(a>=b) return a; return b` — max via if/return, no ternary).
///  - `Grade` (`if(x>=90)4; if(x>=80)3; if(x>=70)2; return 1` — if-elseif ladder, distinct values).
/// (FldTwice `v*v + v` — CORRECT divergence, a NEW class: getfield/redundant-field-load CSE. d8
/// reads `this.v` once and reuses it for all three uses; skotch's straight-line path re-emits the
/// iget per use (correct — no intervening write — but larger). The SSA path has redundant-load
/// CSE; the straight-line path doesn't. Deferred.)
#[test]
fn stress_round52_byte_identical() {
    for name in ["BitNot", "LongNot", "StaticFinal", "FloatChain", "LongArrAr", "MaxIf", "Grade"] {
        assert_byte_identical(name);
    }
}

/// 51st semantic stress round, byte-identical — a clean sweep (8/8, no divergences/bails):
///  - `LongZero` (`if(x == 0L) return 1; return 0` — long compare-to-zero (cmp-long + if-eqz)).
///  - `IsUpper` (`c >= 'A' && c <= 'Z'` — char range test via short-circuit `&&`).
///  - `LenMinus` (`a.length - 1` — array-length minus one).
///  - `StaticChain` (`g(h(x))` — nested static calls).
///  - `NestLen` (`a[i].length` over `int[][]` — aget-object then array-length).
///  - `DblNegAdd` (`-a + b` over double — neg-double + add-double).
///  - `MixShift` (`(x<<1) + (x>>>31)` — left shift plus unsigned-right shift).
///  - `WrapMul` (`x * 1000003` — large-constant multiply (mul-int/lit not applicable → 3-addr)).
#[test]
fn stress_round51_byte_identical() {
    for name in ["LongZero", "IsUpper", "LenMinus", "StaticChain", "NestLen", "DblNegAdd", "MixShift", "WrapMul"] {
        assert_byte_identical(name);
    }
}

/// 50th semantic stress round (milestone), byte-identical — long shift/mask, casts, bit roundtrip:
///  - `LongShMask` (`(x >> 16) & 0xffffL` — long shift then long-mask).
///  - `NegConst` (`x > 0 ? -100 : -200` — ternary over two negative int constants).
///  - `CharRange2` (`(c - 'a') & 0x1f` — char subtraction then mask).
///  - `ByteRound` (`(byte) x + 1` — int→byte narrowing (i2b) then add).
///  - `DblBitsRT` (`Double.longBitsToDouble(Double.doubleToRawLongBits(x))` — nested wide
///    bit-reinterpret roundtrip).
///  - `ArrPS` (`a[0]*a[1] + a[2]*a[3]` — four array loads, two muls + add).
/// (MixParam `a + (int)b + c` (b long) BAILS — l2i 0x88. BoolToInt `b ? 1 : 0` — CORRECT
/// divergence: d8 recognizes the boolean-identity ternary and emits `return v0` (b is already
/// 0/1); skotch emits the full branch. Dual of the boolean-not-xor CFG-diamond-collapse class;
/// deferred (SSA-path pattern-match, risky).)
#[test]
fn stress_round50_byte_identical() {
    for name in ["LongShMask", "NegConst", "CharRange2", "ByteRound", "DblBitsRT", "ArrPS"] {
        assert_byte_identical(name);
    }
}

/// 49th semantic stress round, byte-identical — long/mixed arithmetic, modulo, shift, negation:
///  - `LongChain` (`a*b - c*d` over long — two mul-long + sub-long).
///  - `MixArith` (`c*2 + b` with char c, byte b — mixed-width int arithmetic).
///  - `AndTern` (`(a>0 && b>0) ? a+b : 0` — short-circuit && guarding a ternary value).
///  - `ModCombo` (`a%b + c%d` — two rem-int + add).
///  - `ShiftAdd` (`(x<<2) + x` — shift-and-add (×5 idiom)).
///  - `NegMul` (`-a * b` — unary negate feeding a multiply).
/// (DblCombo `a*b + c/a` and RotShift `(x<<n)|(x>>>(32-n))` — CORRECT commutative-operand-order
/// divergences (mul-double / or-int operands swapped, both write the same value); same size.)
#[test]
fn stress_round49_byte_identical() {
    for name in ["LongChain", "MixArith", "AndTern", "ModCombo", "ShiftAdd", "NegMul"] {
        assert_byte_identical(name);
    }
}

/// 48th semantic stress round, byte-identical — pure arithmetic / boolean logic (no intrinsics):
///  - `SumSq` (`a*a + b*b - c` — sum of squares minus a third operand).
///  - `DiffSq` (`(a+b)*(a-b)` — difference of squares; add and sub feeding a multiply).
///  - `BoolLogic` (`a && b || c` — multi-param short-circuit boolean to a returned boolean).
///  - `ArrMul` (`a[0]*a[1] - a[2]` — three array loads + mul + sub, straight-line).
///  - `CharArith2` (`s.charAt(0)*31 + s.charAt(1)` — charAt arithmetic, hashCode-shaped).
///  - `TernArith2` (`(a>b ? a : b) * 2` — ternary result feeding a multiply).
///  - `NestArith` (`((a+b)*c - a) / (b+1)` — deeply nested parenthesized arithmetic).
/// (CastChain `(short)(int) x` over long BAILS — l2i 0x88 picks the source's HIGH register,
/// deliberately bailed in the straight-line path; safe.)
#[test]
fn stress_round48_byte_identical() {
    for name in ["SumSq", "DiffSq", "BoolLogic", "ArrMul", "CharArith2", "TernArith2", "NestArith"] {
        assert_byte_identical(name);
    }
}

/// 47th semantic stress round, byte-identical — double-const ternary, length compare, char[],
/// static fields:
///  - `DblConstSel` (`x > 0 ? 3.14 : 2.71` — ternary selecting between two double constants).
///  - `LenEq` (`a.length() == b.length()` — two virtual calls feeding an int-equality boolean).
///  - `CharArrR` (`a[0] + a[1]` over `char[]` — two aget-char + add).
///  - `TwoStatic` (`a + b` over two static int fields — two sget + add).
/// (This round's 4 divergences are ALL correct DESUGARING: DblMax/LongMaxR — d8 rewrites
/// `Double.max`/`Long.max` → `Math.max` (API-24 method-ref swap); IsFinite/SubExact — d8
/// synthetic-class backports of `Float.isFinite`/`Math.subtractExact`. skotch emits the faithful
/// invoke-static; correct for API≥24; deferred to a future desugaring pass.)
#[test]
fn stress_round47_byte_identical() {
    for name in ["DblConstSel", "LenEq", "CharArrR", "TwoStatic"] {
        assert_byte_identical(name);
    }
}

/// 46th semantic stress round, byte-identical — parse intrinsics, field array-length, char mask:
///  - `ParseLong` (`Long.parseLong(s)` — invoke-static returning long).
///  - `ParseDouble` (`Double.parseDouble(s)` — invoke-static returning double).
///  - `ParseBool` (`Boolean.parseBoolean(s)` — invoke-static returning boolean).
///  - `FldArrLen` (`items.length` — array-length of an instance field (iget-object + array-length)).
///  - `CharMask` (`(c & 0xff) | 0x20` — char masked then or-ed).
///  - `ReplaceCS` (`s.replace("ab", "cd")` — String.replace(CharSequence,CharSequence)).
/// (LongFldCond `b ? val : 0L` — CORRECT getfield/args-high reg-numbering divergence (FldBool
/// family); same size. IntMax `Integer.max(a,b)` — DESUGARING: d8 REWRITES it to `Math.max(a,b)`
/// (API-24 backport via method-ref swap); skotch keeps Integer.max (longer string → 4 bytes
/// larger). Both correct, deferred.)
#[test]
fn stress_round46_byte_identical() {
    for name in ["ParseLong", "ParseDouble", "ParseBool", "FldArrLen", "CharMask", "ReplaceCS"] {
        assert_byte_identical(name);
    }
}

/// 45th semantic stress round, byte-identical — parse/Math intrinsics, double[] sum, char arith:
///  - `ToRad` (`Math.toRadians(deg)` — single-double-arg invoke-static).
///  - `ParseFloat` (`Float.parseFloat(s)` — invoke-static returning float).
///  - `ParseRadix` (`Integer.parseInt(s, 16)` — two-arg invoke-static).
///  - `DblArrSum` (`a[0] + a[1] + a[2]` over `double[]` — three aget-wide + two add-double).
///  - `CharSub` (`c - '0'` — char minus char-const → int).
///  - `SubOne` (`s.substring(2)` — one-int-arg virtual call → String).
/// (TwoArrRead `if(a[i]>a[i+1])` BAILS — iaload 0x2e in CFG path. FldBool `return x == 0` —
/// CORRECT getfield dead-receiver-reuse divergence (same as FldCmp): skotch reuses dead `this`
/// reg (`iget v0,v0`), d8 keeps it high (`iget v0,v1`); rest identical, same size.)
#[test]
fn stress_round45_byte_identical() {
    for name in ["ToRad", "ParseFloat", "ParseRadix", "DblArrSum", "CharSub", "SubOne"] {
        assert_byte_identical(name);
    }
}

/// 44th semantic stress round, byte-identical — array/field writes, intrinsics, double 3-way:
///  - `Arr2DW` (`a[i][j] = v` over `int[][]` — aget-object then aput).
///  - `LongArrW` (`a[i] = v` over `long[]` — aput-wide).
///  - `IndexOfStr` (`s.indexOf(t)` — virtual call, two object args → int).
///  - `TernCall` (`x > 0 ? g(x) : h(x)` — ternary selecting between two static calls).
///  - `StaticCompound` (`total += x` — sget/add/sput compound on a static field).
///  - `CharDigit` (`Character.digit(c, 16)` — two-arg invoke-static).
///  - `DCmpChain` (`if(a<b) -1; if(a>b) 1; return 0` over `double` — the double sibling of the
///    FloatCmp/LongCmp three-way compare; re-validates the cmp-liveness fix for double here).
/// (ArrFld `return data[i]` — CORRECT array-load reg-reuse divergence: with the array (a freshly
/// loaded field) and the index both dead at the aget, d8 reuses the INDEX register, skotch the
/// ARRAY register; same size. Earlier multi-use-array fixtures matched (array stayed live).)
#[test]
fn stress_round44_byte_identical() {
    for name in ["Arr2DW", "LongArrW", "IndexOfStr", "TernCall", "StaticCompound", "CharDigit", "DCmpChain"] {
        assert_byte_identical(name);
    }
}

/// 43rd semantic stress round (harder/compound shapes), byte-identical:
///  - `Arr2D` (`a[i][j]` over `int[][]` — aget-object then aget; nested array element access).
///  - `LDCmp` (`a < b` with `long a, double b` — long widened (l2d) then cmp-double + branch).
///  - `FourParam` (`a*b + c*d` — instance method, 4 int params + this, two muls + add).
///  - `BitField` (`(x & ~0xff) | (v & 0xff)` — bit-field clear+insert: and + and-mask + or).
///  - `ChainMix` (`Integer.toString(x).substring(1)` — static call result used as a receiver).
/// (CondArrStore `if(v>0) a[i]=v` BAILS — aput 0x4f in CFG path. StrSwitch BAILS — sparse-switch
/// 0xab. Tern3 `a>b?(a>c?a:c):(b>c?b:c)` — CORRECT return-tail-merge divergence: d8 shares one
/// `return v2` across both inner ternaries' else-paths; skotch emits both. Same size; deferred.)
#[test]
fn stress_round43_byte_identical() {
    for name in ["Arr2D", "LDCmp", "FourParam", "BitField", "ChainMix"] {
        assert_byte_identical(name);
    }
}

/// 42nd semantic stress round, byte-identical — a clean sweep (8/8, no divergences/bails):
///  - `LongFld` (`base * x` — long instance-field read (iget-wide) feeding mul-long).
///  - `DblTern` (`a > b ? a : b` over double — double max via ternary).
///  - `LenArith` (`a.length * 2 + 1` — array-length in arithmetic).
///  - `CharTernI` (`c == ' ' ? 0 : 1` — char equality driving an int ternary).
///  - `Reverse` (`Integer.reverse(x)` — invoke-static).
///  - `RevBytes` (`Integer.reverseBytes(x)` — invoke-static).
///  - `CodePoint` (`s.codePointAt(0)` — virtual call → int).
///  - `NextUp` (`Math.nextUp(x)` — single-double-arg invoke-static).
#[test]
fn stress_round42_byte_identical() {
    for name in ["LongFld", "DblTern", "LenArith", "CharTernI", "Reverse", "RevBytes", "CodePoint", "NextUp"] {
        assert_byte_identical(name);
    }
}

/// 41st semantic stress round, byte-identical — field reads, conversions, intrinsics; plus
/// characterizing the iget+iput debug-info divergence:
///  - `FldR` (`return this.count + 1` — iget only, no iput → byte-identical, unlike FldRMW).
///  - `LenToFld` (`this.n = a.length` — iput only (value is array-length), no iget → byte-id).
///  - `DblCmpI` (`a < b ? 1 : 0` over double — cmp-double feeding a ternary).
///  - `InstStatic` (`Math.abs(x)` from an instance method — instance method calling a static).
///  - `IntBitsF` (`Float.intBitsToFloat(x)` — int→float bit-reinterpret intrinsic).
/// CHARACTERIZED: the FldRMW divergence is a DEBUG-INFO encoding difference triggered by a method
/// having BOTH an iget AND an iput (RMW2Fld, RMWBig also diverge; FldR=iget-only and
/// LenToFld=iput-only do NOT). d8's debug_info_item is 2 bytes larger — it emits an extra
/// same-line source position (at the iput) that skotch's line-based dedup collapses. CORRECT
/// (skotch's debug self-validates, dexdump positions identical); deferred — matching d8's nuanced
/// same-line-position heuristic risks the dedup many byte-identical fixtures rely on.
/// (FldReadBr `if(x>this.limit)` BAILS — getfield 0xb4 in CFG path.)
#[test]
fn stress_round41_byte_identical() {
    for name in ["FldR", "LenToFld", "DblCmpI", "InstStatic", "IntBitsF"] {
        assert_byte_identical(name);
    }
}

/// 40th semantic stress round, byte-identical — field/double writes, intrinsics, indexing:
///  - `DblFldW` (`this.d = x * 2.0` — double field write: mul-double + iput-wide).
///  - `GetExp` (`Math.getExponent(x)` — double-arg invoke-static → int).
///  - `FloatSel2` (`if(x>0) return 1.5f; return -1.5f` — float-const select via branch).
///  - `CharArrW` (`a[i] = 'z'` — char-array element store: aput-char).
///  - `UserCall` (`g(x, x+1)` — user static call with a computed second arg).
///  - `BitCountAr` (`Long.bitCount(x) * 2` — intrinsic result feeding a multiply).
///  - `LastChar` (`s.charAt(s.length() - 1)` — length feeds the index of a chained charAt).
/// (FldRMW `this.count = this.count + 1` — CORRECT divergence: dexdump -d is byte-identical
/// (same code, debug, positions, locals) and section sizes match, but a ~2-byte data-section
/// item-ORDERING difference shifts the string-data offset. Pure binary layout; deferred.)
#[test]
fn stress_round40_byte_identical() {
    for name in ["DblFldW", "GetExp", "FloatSel2", "CharArrW", "UserCall", "BitCountAr", "LastChar"] {
        assert_byte_identical(name);
    }
}

/// 39th semantic stress round, byte-identical — field/array writes, intrinsics, multi-call:
///  - `FldWrite` (`this.v = x` — straight-line putfield; works in the straight-line path even
///    though putfield-in-CFG bails).
///  - `ArrStore` (`a[i] = i*2 + 1` — array store of a computed value, straight-line).
///  - `LSignum` (`Long.signum(x)` — wide-arg invoke-static → int).
///  - `NumVal` (`Character.getNumericValue(c)` — char-arg invoke-static → int).
///  - `Rint` (`Math.rint(x)` — single-double-arg intrinsic).
///  - `LongGcd` (`g(x,y) + g(y,x)` — two user wide-arg static calls + add-long).
///  - `StaticWrite` (`counter = x` — sput to a static field).
/// (CharAtBr `if(s.charAt(0)=='x')` BAILS — invoke 0xb6 in CFG path; safe.)
#[test]
fn stress_round39_byte_identical() {
    for name in ["FldWrite", "ArrStore", "LSignum", "NumVal", "Rint", "LongGcd", "StaticWrite"] {
        assert_byte_identical(name);
    }
}

/// `aconst_null` (0x01) in the bootstrap paths: `return null` (and any null constant) bailed —
/// neither the straight-line nor the CFG path handled 0x01. null is `const/4 v,#0` in DEX (an
/// object register holding 0; the `return-object`/store/arg context makes it object-typed, not
/// the const itself), so pushing a plain `Val::ConstInt(0)` materializes byte-identically to
/// d8's null. Added to both paths.
///  - `PureNull` (`return null` — straight-line: const/4 v0,#0 + return-object).
///  - `StrNull` (`if(x>0) return "pos"; return null` — CFG path: the null branch now emits
///    const/4 + return-object instead of bailing).
#[test]
fn aconst_null_byte_identical() {
    for name in ["PureNull", "StrNull"] {
        assert_byte_identical(name);
    }
}

/// 38th semantic stress round, byte-identical — rarer/compound shapes:
///  - `Replace` (`s.replace('a', 'b')` — two-char-arg virtual call returning a String).
///  - `CharStr` (`String.valueOf(c)` — char→String invoke-static).
///  - `LongGet` (`a[0] + a[1]` over `long[]` — two `aget-wide` loads + add-long).
///  - `IsInf` (`Double.isInfinite(x)` — double-arg invoke-static → boolean).
///  - `ParseShort` (`Short.parseShort(s)` — invoke-static, int result narrowed to short on return).
///  - `MultiBool` (`if(a) 1; if(b) 2; return 3` — chain of boolean early-return tests).
/// (StrNull `if(x>0) return "pos"; return null` BAILS — aconst_null 0x01 in the CFG path. BitShift
/// `(x&0xff)<<4 | (x>>8)&0xf` — CORRECT commutative-`or` operand-order divergence (d8 returns v1,
/// skotch v0; same size). Both deferred/safe.)
#[test]
fn stress_round38_byte_identical() {
    for name in ["Replace", "CharStr", "LongGet", "IsInf", "ParseShort", "MultiBool"] {
        assert_byte_identical(name);
    }
}

/// 37th semantic stress round, byte-identical — String/Math/compare intrinsics, &&-chains:
///  - `Concat` (`a.concat(b)` — explicit String.concat virtual call, NOT invokedynamic).
///  - `RoundF` (`Math.round(x)` over `float` — float→int intrinsic).
///  - `AndVal` (`if(a>0 && b>0 && c>0) 1 else 0` — three-way short-circuit `&&`).
///  - `FCompare` (`Float.compare(a, b)` — like Double.compare, d8 does NOT inline to cmp-float
///    (NaN/-0.0 ordering), keeps invoke-static → matches skotch).
///  - `BoolStr` (`Boolean.toString(b)` — invoke-static returning a String).
///  - `IntBin` (`Integer.toBinaryString(x)` — invoke-static returning a String).
///  - `FloorI` (`(int) Math.floor(x)` — double intrinsic + double→int narrowing).
/// (TwoFldCmp `x > y` over two instance fields — CORRECT getfield/args-high reg-reuse divergence
/// (same as FldCmp): skotch reuses dead `this` reg [2 regs], d8 keeps it high [3 regs]; same size.)
#[test]
fn stress_round37_byte_identical() {
    for name in ["Concat", "RoundF", "AndVal", "FCompare", "BoolStr", "IntBin", "FloorI"] {
        assert_byte_identical(name);
    }
}

/// 36th semantic stress round, byte-identical — virtual calls, intrinsics, double[]/long fields:
///  - `ObjHash` (`o.hashCode()` — virtual call on Object → int).
///  - `LenChar` (`s.length() * s.charAt(0)` — two String calls feeding a multiply).
///  - `DblGet` (`a[0] + a[1]` over `double[]` — two `aget-wide` loads + add-double).
///  - `NLZ` (`Integer.numberOfLeadingZeros(x)` — invoke-static).
///  - `EqIgnore` (`a.equalsIgnoreCase(b)` — virtual call, two object args → boolean).
/// (AddExact `Math.addExact(a,b)` — DESUGARING divergence (d8 backports the API-24 method to a
/// synthetic class; skotch invoke-static). InstLong `base + x` (long field + int) BAILS — i2l
/// widening. LongCond2 `if(x>0L) return x; return 0L` BAILS — CFG shared-exit merge shape for a
/// wide return not yet supported. All deferred/safe.)
#[test]
fn stress_round36_byte_identical() {
    for name in ["ObjHash", "LenChar", "DblGet", "NLZ", "EqIgnore"] {
        assert_byte_identical(name);
    }
}

/// 35th semantic stress round, byte-identical — String/Long intrinsics, deep nesting, byte[]:
///  - `StrEmpty` (`s.isEmpty()` — straight-line virtual call → boolean).
///  - `StrHash` (`s.hashCode()` — straight-line virtual call → int).
///  - `ThreeDeep` (`Integer.parseInt(s.trim().substring(1))` — three nested calls, each result
///    feeding the next as receiver/arg).
///  - `ByteGet` (`a[0] + a[1]` over `byte[]` — two `aget-byte` sign-extending loads + add; the
///    baload check routes this through the SSA path).
///  - `LongBin` (`Long.toBinaryString(x)` — wide-arg invoke-static returning a String).
/// (CondFld `if(x>0) v=x` BAILS — putfield 0xb5 in CFG path. SixArg `g(1..6)` BAILS — 6-arg
/// invoke needs 3rc range form. FiveArg `g(1..5)` — CORRECT 35c-invoke reg-numbering divergence:
/// d8's allocator places args 4,5 in v0,v1 and 1,2,3 in v2,v3,v4; skotch assigns v0..v4
/// sequentially. Same size; deferred.)
#[test]
fn stress_round35_byte_identical() {
    for name in ["StrEmpty", "StrHash", "ThreeDeep", "ByteGet", "LongBin"] {
        assert_byte_identical(name);
    }
}

/// 34th semantic stress round (harder shapes), byte-identical:
///  - `BothDie` (`(a+1)-(b+2)` — two lit-folded adds whose temps both die at the subtract).
///  - `LTernArith` (`(a>b ? a : b) + 1L` — long ternary result feeding a wide add).
///  - `NanCmp` (`x != x` over `double` — NaN self-inequality; exercises cmpg/cmpl + if-nez).
/// (This round's harder probes surfaced CORRECT divergences, all deferred: SelfFld `next.v + v`
/// = getfield-result register assignment (d8 reuses dead receiver reg, skotch fresh-then-reuse,
/// same size); ArrSwap `t=a[0];a[0]=a[1];a[1]=t` = const-CSE of the index 0/1 (d8 keeps them in
/// regs, skotch re-materializes); TernSame `x<0?0:(x>100?0:x)` = const-hoist (d8 shares one
/// `const 0` across both return-0 paths). Safe bails: ManyTmp (multi-use locals → register
/// pressure), MixIL (i2l widening).)
#[test]
fn stress_round34_byte_identical() {
    for name in ["BothDie", "LTernArith", "NanCmp"] {
        assert_byte_identical(name);
    }
}

/// 33rd semantic stress round, byte-identical — a clean sweep (8/8, no divergences/bails):
///  - `Atan2` (`Math.atan2(y, x)` — two-double-arg invoke-static).
///  - `TwoFld` (`a + b` over two instance int fields — two igets + add on `this`).
///  - `LongMod` (`x % 1000L` — long remainder by a non-power-of-two const → rem-long).
///  - `CharTern` (`x > 0 ? 'p' : 'n'` — ternary materializing a char constant).
///  - `StrCmp` (`a.compareTo(b)` — virtual call, two object args, int result).
///  - `HighBit` (`Integer.highestOneBit(x)` — invoke-static).
///  - `ByteRet` (`(byte)(x*2+1)` — arithmetic then int→byte narrowing on return).
///  - `Cbrt` (`Math.cbrt(x)` — single-double-arg invoke-static).
#[test]
fn stress_round33_byte_identical() {
    for name in ["Atan2", "TwoFld", "LongMod", "CharTern", "StrCmp", "HighBit", "ByteRet", "Cbrt"] {
        assert_byte_identical(name);
    }
}

/// 32nd semantic stress round, byte-identical — Math intrinsics, division, bit-conversions:
///  - `Pow2` (`Math.pow(base, exp)` — two-double-arg invoke-static).
///  - `Div3` (`x/3 + x%7` — integer division/remainder by non-powers-of-two; d8 keeps
///    `div-int`/`rem-int`, no magic-number strength-reduction).
///  - `FBits` (`Float.floatToIntBits(x)` — float→int bit-reinterpret intrinsic).
///  - `DBits` (`Double.doubleToLongBits(x)` — double→long bit-reinterpret intrinsic).
///  - `Trig` (`Math.sin(x) * Math.cos(x)` — two single-double-arg intrinsics + mul-double).
///  - `HexDigit` (`(c>='0'&&c<='9') || (c>='a'&&c<='f')` — nested char-range &&/|| chain).
///  - `Exp` (`Math.exp(x) - 1.0` — single-double-arg intrinsic + const-wide + sub-double).
/// (LShiftC — `(x<<8)|(x>>>56)` over long — is a CORRECT commutative-`or` operand-order
/// divergence: d8 `or-long/2addr v3,v0` returns v3; skotch `or-long/2addr v0,v3` returns v0.
/// Same size; deferred.)
#[test]
fn stress_round32_byte_identical() {
    for name in ["Pow2", "Div3", "FBits", "DBits", "Trig", "HexDigit", "Exp"] {
        assert_byte_identical(name);
    }
}

/// 31st semantic stress round, byte-identical — double intrinsics, char widening, compare:
///  - `Hypot` (`Math.hypot(a, b)` — two-double-arg invoke-static).
///  - `DblCompare` (`Double.compare(a, b)` — d8 does NOT inline this to `cmp-double` (unlike
///    `Long.compare`→`cmp-long`) because Double.compare's NaN/-0.0 ordering differs from the
///    cmp-double op; it keeps the invoke-static, which is exactly what skotch emits → match).
///  - `CharWiden` (`c + 1` — implicit char→int widening feeding an add).
///  - `Log` (`Math.log(x) + Math.log10(x)` — two single-double-arg intrinsics + add-double).
/// (DblRet `if(x>0) return x; return -x` BAILS — dneg 0x77 in CFG path. StrLenBr
/// `if(s.length()>5)` BAILS — invoke 0xb6 in CFG path. ToIntExact — `Math.toIntExact(x)` — is a
/// DESUGARING divergence (d8 backports the API-24 method to a synthetic class; skotch emits
/// invoke-static). FldCmp — `x > this.threshold` — is a CORRECT reg-reuse divergence: skotch's
/// SSA path reuses the dead `this` register for the iget result (2 regs); d8 uses a fresh reg +
/// args-high (3 regs). All deferred.)
#[test]
fn stress_round31_byte_identical() {
    for name in ["Hypot", "DblCompare", "CharWiden", "Log"] {
        assert_byte_identical(name);
    }
}

/// 30th semantic stress round, byte-identical — intrinsics over int/char/long/double, nesting:
///  - `Signum` (`Integer.signum(x)` — invoke-static returning int).
///  - `ToUpper` (`Character.toUpperCase(c)` — char-in/char-out intrinsic).
///  - `LongMax` (`Math.max(a, b)` over `long` — wide-arg/wide-result invoke-static).
///  - `NestedCall` (`Integer.parseInt(s.trim())` — a call whose arg is another call's result).
///  - `CharClass` (`Character.isLetterOrDigit(c)` — char-arg invoke-static returning boolean).
///  - `CopySign` (`Math.copySign(a, b)` — two-double-arg invoke-static).
/// (LongCompare — `Long.compare(a,b)` — is a CORRECT intrinsic-inline divergence: d8 inlines
/// it to a single `cmp-long v0,v1,v3` (the DEX op IS Long.compare's -1/0/1 semantics); skotch
/// emits invoke-static Long.compare. BitCountBr — `if(Integer.bitCount(x)>2)` — BAILS: invoke
/// 0xb8 in the CFG path. Both deferred.)
#[test]
fn stress_round30_byte_identical() {
    for name in ["Signum", "ToUpper", "LongMax", "NestedCall", "CharClass", "CopySign"] {
        assert_byte_identical(name);
    }
}

/// `array-length` (0xbe) in the BOOTSTRAP CFG path: a branchy method that compares array
/// lengths (`if(a.length > b.length) …`) used to bail (the CFG opcode table lacked 0xbe). The
/// CFG loop now emits `array-length vDest, vArr` (12x), reusing the array's register for the
/// result when the array is dead afterward — matching d8's `array-length v0, v0`. "Dead" is a
/// temp Reg, or a Local neither live-out NOR re-loaded later in-block (a boundary-respecting
/// scan), so a live array is never clobbered.
///  - `ArrLen` (`if(a.length > b.length) return 1; return 0` — two array-lengths, each array
///    dead after its length → both reuse their register).
///  - `ArrLen2` (`if(a.length > 0) return a.length; return 0` — `a` is LIVE across the branch:
///    block 0 takes a fresh dest (a live-out), block 1 reuses a's register (a dead). The
///    in-block + live-out dead-check reproduces d8 exactly — a regression guard against
///    clobbering a still-needed array.)
#[test]
fn array_length_in_cfg_path_byte_identical() {
    for name in ["ArrLen", "ArrLen2"] {
        assert_byte_identical(name);
    }
}

/// 29th semantic stress round, byte-identical — chained calls, intrinsics, static field read:
///  - `TrimLen` (`s.trim().length()` — chained virtual calls, result of one feeds the next).
///  - `Round` (`Math.round(x)` — double→long intrinsic, wide result from a narrow-ish call).
///  - `IsNaN` (`Float.isNaN(x)` — float-arg invoke-static returning boolean).
///  - `StaticFld` (`x + COUNT` with `static int COUNT = 42` — sget of a static field that
///    carries a constant static_value, plus the field add).
///  - `CeilFloor` (`Math.ceil(x) - Math.floor(x)` — two double intrinsics + sub-double).
///  - `ValOf` (`String.valueOf(x)` — invoke-static returning an object).
/// (ArrLen `a.length>b.length` BAILS — `array-length` 0xbe in the CFG path; LongBitBr
/// `(flags&mask)!=0L` BAILS — `land` 0x7f long-bitwise in the CFG path. Both are CFG-path
/// opcode gaps, safe, future contained opportunities alongside binops/invoke.)
#[test]
fn stress_round29_byte_identical() {
    for name in ["TrimLen", "Round", "IsNaN", "StaticFld", "CeilFloor", "ValOf"] {
        assert_byte_identical(name);
    }
}

/// 28th semantic stress round, byte-identical — String/Math intrinsics, multi-arg calls,
/// char arithmetic, double-compare branch:
///  - `SubStr` (`s.substring(a, b)` — two-int-arg virtual call returning a String).
///  - `DMax` (`Math.max(a, b)` over `double` — wide-arg invoke-static).
///  - `LAbs` (`Math.abs(x)` over `long` — wide-arg/wide-result invoke-static).
///  - `Clamp2` (`Math.max(lo, Math.min(v, hi))` — nested 3-distinct-arg static calls).
///  - `IndexOf` (`s.indexOf('x')` — char-arg virtual call returning int).
///  - `CharArith` (`(a-'0')*10 + (b-'0')` — char arithmetic chain, straight-line).
///  - `DblBr` (`if(x>=0.0) return 1; return -1` — double compare against a const driving a branch).
/// (NotEmpty — `!s.isEmpty()` — is a CORRECT missing-opt divergence: d8 folds the
/// boolean-negation branch into `xor-int/lit8 v,#1`; skotch emits the ifne/const/goto/const
/// branch. A recurring `!bool` peephole worth a future iteration; deferred.)
#[test]
fn stress_round28_byte_identical() {
    for name in ["SubStr", "DMax", "LAbs", "Clamp2", "IndexOf", "CharArith", "DblBr"] {
        assert_byte_identical(name);
    }
}

/// 27th semantic stress round, byte-identical — String/Math intrinsics, bit ops, straight-line:
///  - `StrCharAt` (`s.charAt(0) + s.length()` — two String calls + add, straight-line).
///  - `Sqrt` (`Math.sqrt(x) + Math.sqrt(x+1.0)` — double intrinsic twice + add-double).
///  - `IntToStr` (`Integer.toString(x)` — invoke-static returning an object).
///  - `LTrailing` (`Long.numberOfTrailingZeros(x)` — wide-arg intrinsic returning int).
///  - `RotL` (`Integer.rotateLeft(x, n)` — two-int-arg bit-rotate intrinsic).
///  - `StrEqB` (`s.equals("ok")` — straight-line const-string + invoke-virtual → boolean).
/// (NestIf2 — `if(a>0){ if(b>0) return a+b; return a; } return -1` — BAILS: the CFG path has
/// no arithmetic binops (`iadd` 0x60), a contained future opportunity like ldc was. LongTern —
/// a long ternary chain — is a CORRECT return-tail-merge divergence: d8 collapses the two
/// `return c` into one shared exit; skotch emits both. Same size; deferred.)
#[test]
fn stress_round27_byte_identical() {
    for name in ["StrCharAt", "Sqrt", "IntToStr", "LTrailing", "RotL", "StrEqB"] {
        assert_byte_identical(name);
    }
}

/// `ldc`/`ldc_w`/`ldc2_w` (string/float/long/double constants) in the BOOTSTRAP CFG path: a
/// branchy method that loads a constant on a path (`if(x>0) return "pos"; return "neg"`) used
/// to bail (the CFG opcode table lacked 0x12–0x14, though the straight-line path had them).
/// The CFG loop now pushes the same lazy `Val::Const*` and materializes it via cfg_materialize,
/// matching the straight-line path. String-or-float select dispatch is common.
///  - `StrSel2` (`if(x>0) return "pos"; return "neg"` — const-string on each branch).
///  - `FloatSel` (`if(x>0) return 3.5f; return 1.5f` — ldc float bit-pattern per branch).
/// (`LongSel`, a long-const select, is a CORRECT divergence — a wide result beside a narrow
/// arg lays the args-high registers out differently than d8, same size; left deferred.)
#[test]
fn ldc_in_cfg_path_byte_identical() {
    for name in ["StrSel2", "FloatSel"] {
        assert_byte_identical(name);
    }
}

/// A string load (`const-string`) is a position-bearing instruction in d8 (string resolution
/// may throw), so d8 records a source position at it — anchoring the debug line table at the
/// const-string's address. skotch previously recorded positions only at invoke/field/div, so
/// `return "hi"` either dropped the debug_info_item entirely (no throwing op) or deferred the
/// line to the next throwing op (`0x0002 line=N` instead of d8's `0x0000 line=N`). Recording
/// a position at `emit_const_string` fixes both — these are now byte-identical including debug
/// info (the only divergence was a single debug special-opcode byte / a missing debug_info).
///  - `JustStr` (`return "hi"` — const-string + return-object, NO other throwing op).
///  - `StartsW` (`s.startsWith("http")` — const-string at addr 0 before the invoke at addr 2).
#[test]
fn const_string_debug_position_byte_identical() {
    for name in ["JustStr", "StartsW"] {
        assert_byte_identical(name);
    }
}

/// 26th semantic stress round, byte-identical — loops + intrinsics + power-of-two arithmetic:
///  - `DoWhile2` (`do { s += i; i++; } while(i<n)` — do/while: the condition test is at the
///    loop BOTTOM, a back-edge with no separate pre-header test).
///  - `LongPow2` (`x/8L + x%16L` — long division and remainder by powers of two; d8 keeps the
///    `div-long`/`rem-long` rather than strength-reducing to shifts/masks for signed values).
///  - `IsDigit` (`for(i) if(Character.isDigit(s.charAt(i))) c++` — invoke-static intrinsic
///    fed by charAt, driving a conditional increment in a loop).
#[test]
fn stress_round26_byte_identical() {
    for name in ["DoWhile2", "LongPow2", "IsDigit"] {
        assert_byte_identical(name);
    }
}

/// 25th semantic stress round, byte-identical — dispatch chains, bit tests, intrinsics,
/// double three-way compare:
///  - `CharDisp` (`if(c=='+')0; if(c=='-')1; …; -1` — char if-ladder dispatch, early returns).
///  - `LBitCount` (`Long.bitCount(x)` — wide-arg invoke-static intrinsic returning int).
///  - `NestedCond` (`if(a){ if(b) return 1; } return 0` — nested `if` with a shared exit).
///  - `MaskTest` (`(flags & mask) != 0` — bit-test producing a boolean).
///  - `DCmp3w` (`a<b ? -1 : (a>b ? 1 : 0)` over `double` — nested ternary on a double compare;
///    re-confirms the wide-cmp result lands in a fresh register below the wide param pairs).
///  - `ShiftMask` (`(x >> s) & 0xff` — variable shift then and-mask, byte extraction).
/// (Also surfaced TernArith — `(a>b?1:0)+(c>d?1:0)` — a CORRECT divergence where skotch's
/// per-ternary lowering is one unit TIGHTER than d8's const-hoisting SSA lowering; left as-is.)
#[test]
fn stress_round25_byte_identical() {
    for name in ["CharDisp", "LBitCount", "NestedCond", "MaskTest", "DCmp3w", "ShiftMask"] {
        assert_byte_identical(name);
    }
}

/// 24th semantic stress round, byte-identical — intrinsics, nested ternaries, char ranges:
///  - `Min3` (`m = a<b?a:b; m<c?m:c` — two chained ternaries, three-way minimum).
///  - `MathMin` (`Math.min(a, Math.min(b, c))` — nested invoke-static intrinsic).
///  - `CharRange` (`c>='a' && c<='z'` — char range test: two `if-*z` short-circuit `&&`).
///  - `AbsSub` (`Math.abs(a - b)` — subtract feeding an intrinsic call).
///  - `SignMul` (`Integer.signum(a) * b` — intrinsic result feeding a multiply).
/// (This round also confirmed a spread of CORRECT divergences left deferred: commutative
/// 2addr operand-order on the final `or`/`xor` of `(a&b)|(a^b)` and `(v<<k)|(v>>>k)`;
/// 2addr-vs-3addr register choice on long shifts; const-CSE of a repeated `1.0`; return
/// tail-merge of two identical `return v0`; ternary-branch register unification; and the
/// `Math.floorMod`/`floorDiv` desugaring backport — none of them miscompiles.)
#[test]
fn stress_round24_byte_identical() {
    for name in ["Min3", "MathMin", "CharRange", "AbsSub", "SignMul"] {
        assert_byte_identical(name);
    }
}

/// 21st semantic stress round, byte-identical — bit manipulation + arithmetic idioms:
///  - `S21Pack` (`(hi<<8) | (lo&0xff)` — byte packing: shift + and-mask + or).
///  - `S21Unpack` (`(x>>(i*8)) & 0xff` — variable shift + and-mask).
///  - `S21Gray` (`n ^ (n>>>1)` — binary→Gray code).
///  - `S21Round` (`(x+m-1)/m*m` — round up to a multiple).
///  - `S21Prod` (`long p=1; for(i) p*=i` — factorial, wide product).
#[test]
fn stress_round21_byte_identical() {
    for name in ["S21Pack", "S21Unpack", "S21Gray", "S21Round", "S21Prod"] {
        assert_byte_identical(name);
    }
}

/// Bootstrap-path shift-by-constant lit-fold: `x << c` / `x >> c` / `x >>> c` in a
/// straight-line method → `shl/shr/ushr-int/lit8` (the bootstrap `int_binop` lit-folds via
/// lit_ops, which excludes shifts, so this is handled separately — mirrors the SSA path).
/// `BShr` has `x>>3` / `x<<5` / `x>>>7`.
#[test]
fn bootstrap_shift_lit_fold_byte_identical() {
    assert_byte_identical("BShr");
}

/// 20th semantic stress round, byte-identical — ternary-heavy (the newly SSA-routed path):
///  - `S20BoolAnd` (`return a>0 && b>0` — boolean `&&` result).
///  - `S20TernCall` (`s += c ? g(i) : i` — ternary selecting a call vs a value, in a loop).
#[test]
fn stress_round20_byte_identical() {
    for name in ["S20BoolAnd", "S20TernCall"] {
        assert_byte_identical(name);
    }
}

/// Straight-line ternaries / boolean-from-compare, byte-identical to d8. javac compiles
/// these with a `goto` (0xa7), which the CFG bootstrap path bails on; they now route to
/// the SSA path (method_has_goto → needs_ssa) which handles goto byte-identically.
///  - `MaxT` (`a>b ? a : b`), `AbsT` (`x>=0 ? x : -x` — single-operand if-gez, no
///    over-coalesce), `BoolR` (`return a > b` — boolean materialized from a compare),
///    `SignT` (`x>0 ? 1 : (x<0 ? -1 : 0)` — nested ternary).
#[test]
fn straight_line_ternary_byte_identical() {
    for name in ["MaxT", "AbsT", "BoolR", "SignT"] {
        assert_byte_identical(name);
    }
}

/// 19th semantic stress round, byte-identical — fresh idioms:
///  - `S19ShAcc` (`h = (h<<5) - h + a[i]` — djb2 `h*33` written as shift+sub).
///  - `S19Clamp3` (`Math.min(Math.max(x,0),255)` — nested static clamp to byte range).
///  - `S19LongV` (`s += ((long)a[i]) << (i&31)` — long shift by a variable amount).
///  - `S19Hypot` (`s += x[i]*x[i] + y[i]*y[i]` — two arrays, squared sums).
#[test]
fn stress_round19_byte_identical() {
    for name in ["S19ShAcc", "S19Clamp3", "S19LongV", "S19Hypot"] {
        assert_byte_identical(name);
    }
}

/// Reverse-subtract lit-fold, matching d8: `c - x` (constant minus variable) → DEX's
/// `rsub-int/lit8 x,#c` (no plain sub-const exists; `x - c` folds as add-neg). `RsubL`
/// (`s += 100 - a[i]`) exercises the SSA loop path; `RsubS` (`return 100 - x`) the
/// straight-line BOOTSTRAP path.
#[test]
fn rsub_const_lit_fold_byte_identical() {
    for name in ["RsubL", "RsubS"] {
        assert_byte_identical(name);
    }
}

/// The bootstrap path records a debug LINE POSITION at a div/rem even when it's lit-folded
/// by a constant (`div-int/lit8 #100`) — d8 treats div/rem as throwing in its IR before
/// noticing the literal divisor is non-zero. (Previously only the register-div path
/// recorded it, so single-line methods ending in `/100` had empty `positions` and a
/// smaller debug_info than d8.) `RsubInterp` (`(a*(100-t)+b*t)/100`) and `S14Lerp`
/// (`a+(b-a)*t/100`) are now byte-identical.
#[test]
fn bootstrap_div_lit_debug_position_byte_identical() {
    for name in ["RsubInterp", "S14Lerp"] {
        assert_byte_identical(name);
    }
}

/// 18th semantic stress round, byte-identical — fresh idioms + const/register stressors:
///  - `S18MulAdd` (`s += a[i]*2 + a[i]*3` — repeated `a[i]` CSE'd + two const muls).
///  - `S18LowBit` (`n & -n` — lowest set bit via negate + and).
///  - `S18Swap` (`int t=a[i]; a[i]=a[j]; a[j]=t` — array-element swap).
#[test]
fn stress_round18_byte_identical() {
    for name in ["S18MulAdd", "S18LowBit", "S18Swap"] {
        assert_byte_identical(name);
    }
}

/// A constant used by BOTH a lit-foldable binop AND a register-requiring use (here `k=3`
/// as an array INDEX `a[k]` and a binop operand `+k`) used to SILENTLY MISCOMPILE:
/// `is_rematerialized` only inspected binop uses, so it rematerialized the const (NO
/// register) and the array-index operand emitted garbage (`aget v2, v3, v253` — NO_REG
/// truncated). Fixed: is_rematerialized rejects a const with ANY non-foldable use; plus a
/// build_dex safety net (regalloc::max_register_used) bails on any out-of-range register
/// operand. `RematK` must now dex correctly and self-validate (k lands in a register).
#[test]
fn const_with_register_use_not_rematerialized() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("RematK.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let p = dex_classes(&[cf], &opts).expect("RematK must dex (k gets a register), not bail");
    skotch_dex::validator::validate(&p).expect("RematK self-validates (no NO_REG operand)");
}

/// Ternary running-min/max with a temp (`int x=a[i]; m = x<m ? x : m`) over-coalesces via a
/// TRANSITIVE merge where the loop-φ `m` inherits the ternary φ's group (already holding the
/// load `x`), putting `x` and `m` — both live at `x<m` — into one register (`if-le v0,v0`).
/// The φ-coalescer's operand-group interference check fixes the SIBLING shape (two φs sharing
/// an operand) but NOT this one (the conflict is with v.id's INHERITED group, which can't be
/// over-blocked without breaking legitimate handler/loop-φ coalescing — that needs value-aware
/// / Sreedhar-style interference). So these still BAIL (never miscompile, caught by the
/// complete post-alloc guard). bail beats miscompile.
///  - `XbTmp` (`m = x>m ? x : m`), `S11Min` (`m = x<m ? x : m`), `S20MinT`.
#[test]
fn over_coalesced_ternary_still_bails() {
    for name in ["XbTmp", "S11Min", "S20MinT"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let err = dex_classes(&[cf], &opts).expect_err("must bail, not miscompile");
        assert!(format!("{err:#}").contains("over-coalesce"), "{name}: unexpected bail: {err:#}");
    }
}

/// dup_x2 (0x5b) and dup2_x1 (0x5d) operand-stack ops — the SSA stack simulator (`sim_block`)
/// and IR builder (`build_ssa`) now model both (per the JVM spec's category-1 and category-2
/// forms) as pure value-stack reorders. `ArtDupX` exercises `ia[i] += v` (dup_x2 form 1) and
/// `this.longField/doubleField += v` returning the result (dup2_x1 form 2). These were the
/// largest ssa-sim opcode bucket after monitorenter; ~11 guava methods now dex. Runtime
/// correctness (the shuffle is right) is proven on ART by `tests/art/ArtDupX`.
#[test]
fn dup_x2_and_dup2_x1_now_dex() {
    let cf = skotch_classfile::parse_class_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtDupX.class"),
    )
    .unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtDupX (dup_x2 + dup2_x1) should dex");
    skotch_dex::validator::validate(&dex).expect("ArtDupX dex must validate");
}

/// `synchronized` blocks. javac compiles them to monitor-enter/monitor-exit plus an implicit
/// SELF-COVERING catch-all handler (`[handler, handler_end) → handler`) so the handler's own
/// monitor-exit re-runs if it throws. The SSA path now models monitor-enter (0x1d) / monitor-exit
/// (0x1e) and DROPS that self-covering region (functionally exact — the monitor is held when the
/// handler runs, so its monitor-exit can't throw), leaving the body's normal try/catch. The
/// monitorenter bucket was the largest single ssa-sim opcode; ~80 corpus methods now dex. Runtime
/// correctness AND no-deadlock are proven on ART by `tests/art/ArtSync`.
#[test]
fn synchronized_monitor_now_dexes() {
    let cf = skotch_classfile::parse_class_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtSync.class"),
    )
    .unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtSync (synchronized) should dex");
    skotch_dex::validator::validate(&dex).expect("ArtSync dex must validate");
}

/// A CAPTURING bound method reference whose target takes a primitive while the SAM gives a boxed
/// value (`s::charAt` as `Function<Integer,Character>`): the synthetic capturing SAM now unboxes
/// the boxed parameter in place (invoke-virtual intValue + move-result) before forwarding to the
/// impl, mirroring the non-capturing param-unbox. Runtime correctness is proven on ART by
/// `tests/art/ArtCapUnbox`.
#[test]
fn lambda_capturing_param_unbox_now_dexes() {
    let cf = skotch_classfile::parse_class_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtCapUnbox.class"),
    )
    .unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtCapUnbox (capturing param unbox) should dex");
    skotch_dex::validator::validate(&dex).expect("ArtCapUnbox dex must validate");
}

/// A capturing lambda that captures a WIDE (long/double) value: the synthetic class now gets a
/// long/double field (a 2-register pair), the ctor iput-wide's it, and the SAM iget-wide's it and
/// forwards BOTH register halves to the impl. Register layout is per-capture variable width.
/// Runtime correctness (the wide register pairs are right) is proven on ART by `tests/art/ArtWideCap`.
#[test]
fn lambda_wide_capture_now_dexes() {
    let cf = skotch_classfile::parse_class_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/art/ArtWideCap.class"),
    )
    .unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).expect("ArtWideCap (wide capture) should dex");
    skotch_dex::validator::validate(&dex).expect("ArtWideCap dex must validate");
}

/// 11th semantic stress round, byte-identical — fresh idioms + over-coalesce-net probes
/// that DON'T over-coalesce:
///  - `S11Max3` (`m=a; if(b>m)m=b; if(c>m)m=c` — chained max via plain ifs, no ternary φ).
///  - `S11Call2` (`s += g(i, i*2)` — two-argument static call in a loop).
///  - `S11SumD` (`while(n>0){ s+=n%10; n/=10; }` — digit sum).
///  - `S11Pop` (`while(n!=0){ c++; n&=n-1; }` — Kernighan popcount).
///  - `S11Clamp` (`if(x<lo)x=lo; if(x>hi)x=hi; s+=x` — clamp; the original operand-operand
///    interference fix keeps x/lo/hi distinct, no over-coalesce).
#[test]
fn stress_round11_byte_identical() {
    for name in ["S11Max3", "S11Call2", "S11SumD", "S11Pop", "S11Clamp"] {
        assert_byte_identical(name);
    }
}

/// SSA-path checkcast (`check-cast`), instanceof (`instance-of`), and `aconst_null`
/// (`const/4 v,0`), plus object-array stores (`aput-object`, 0x4d — NOT 0x4e
/// aput-boolean, a latent JVM→DEX mapping bug this exposed). These bodies bailed
/// "ssa: unsupported opcode 0xc0/0xc1" before; now they dex and self-validate.
/// `ArtTypes` (the loop-driven version) is also run end-to-end on ART
/// (tests/art/ArtTypes) — that device run is the regression for the aput-object fix,
/// since pre-fix the `Object[]` init failed verification ("put insn has type Boolean").
#[test]
fn ssa_type_ops_and_object_array_now_dex() {
    for name in ["ArtTypes", "ObjArrStore"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts)
            .unwrap_or_else(|e| panic!("{name}: type-ops/object-array should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// SSA-path `athrow` (`throw vAA`, 0x27) via `Terminator::Throw` — both an uncaught-
/// within-method throw that propagates out AND an athrow inside a try (caught locally).
/// Fixed two real miscompiles this exposed: (1) `dex_insn_len(0x27)` was 2 (throw is
/// 11x = 1 word), so `remap_insns` skipped — and left UN-remapped — the instruction
/// right after every throw (a register clobber); (2) the throw terminator wasn't added
/// to `throw_spans`, so the try_item didn't cover the raising instruction and the
/// exception escaped the catch. `ArtThrow` runs end-to-end on ART (tests/art/ArtThrow,
/// stdout `6,-1,10,-1,g=15,-3`) — that device run is the real regression.
#[test]
fn ssa_athrow_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtThrow.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtThrow: athrow should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtThrow: invalid dex: {e:#}"));
}

/// SSA-path reference-equality branches: if_acmpeq (0xa5) / if_acmpne (0xa6), the
/// object `==`/`!=` compares. Wired through `cond_branch_dex_op` (→ if-eq 0x32 /
/// if-ne 0x33, 22t two-register), which `split_blocks_with` keys off for both leader
/// and successor detection, so the SSA CFG picks them up automatically. `ArtRefEq`
/// runs on ART (tests/art/ArtRefEq, stdout `Ss,Ss,Dd,Dd,`).
#[test]
fn ssa_ref_equality_branches_now_dex() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtRefEq.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtRefEq: if_acmp should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtRefEq: invalid dex: {e:#}"));
}

/// SSA FALLBACK routing: `dex_method` now tries the bootstrap straight-line/CFG path
/// first (stays byte-identical for what it handles) and FALLS BACK to the SSA pipeline
/// when it bails — so acyclic methods with invoke/field/new/arith/array-literal that
/// the bootstrap path can't model now dex instead of bailing. This took dex-OK from
/// gson 61%→72% / kstdlib 68%→77%. `ArtFallback` (array literal + double div + l2i +
/// branch-with-call) runs on ART (tests/art/ArtFallback, stdout `10-20-30,3.5,1,10-14`).
#[test]
fn ssa_fallback_acyclic_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtFallback.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtFallback: should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtFallback: invalid dex: {e:#}"));
}

/// tableswitch (0xaa) + lookupswitch (0xab) now DEX (both paths bailed before). Lowered
/// functional-correctly (NOT d8's packed/sparse-switch payload) to a `const tmp,k;
/// if-eq key,tmp,case` chain + `goto default` via a reserved scratch register.
/// `split_blocks_with` + `parse_switch` build the multi-target CFG; `Terminator::Switch`
/// carries it; the emit phase lowers the chain. `ArtSwitch` runs on ART
/// (tests/art/ArtSwitch, stdout `zero,one,two,many,10-20-30--1`).
#[test]
fn ssa_switch_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtSwitch.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtSwitch: switch should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtSwitch: invalid dex: {e:#}"));
}

/// `swap` (0x5f) — exchange the top two category-1 operand-stack values; a pure value-
/// stack reorder in the SSA builder (no instruction emitted), strictly simpler than the
/// already-validated `dup_x1`/`dup_x2`. Kotlin value-class `equals` over an array emits
/// it (operand reorder for `Arrays.equals`); `SwapVKt`/`Buf` are `kotlinc`-compiled from
/// the bundled `SwapVKt.kt.txt`. NOTE: swap has no standalone ART fixture (the only
/// emitters are Kotlin value-classes, which reference `Intrinsics` — itself un-dexable
/// here because it uses `ldc` class constants), so it's validated by dex+self-validate +
/// correctness-by-construction + the 15 kstdlib classes it unblocked (79.0%→80.6%).
#[test]
fn ssa_swap_now_dexes() {
    for name in ["SwapVKt", "Buf"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: swap should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// `const-class` (0x1c) — the `X.class` literal (`ldc`/`ldc_w` of a CONSTANT_Class).
/// New `SsaOp::ConstClass`; emits `const-class dest, type@` (throwing, a `Class` ref).
/// Also added 0x1c to regalloc reg_fields (it was MISSING — would have left the result
/// register un-remapped args-high, the same class of bug as throw/0x27). High cascade:
/// unblocked class-literal/reflection users incl. `kotlin.jvm.internal.Intrinsics`
/// (gson 73.4%→83.9%, kstdlib 80.6%→83.5%). `ArtClassLit` runs on ART
/// (tests/art/ArtClassLit, stdout `ArtClassLit,java.lang.String,true,false,[I`,
/// incl. the array-class literal `int[].class`).
#[test]
fn ssa_const_class_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtClassLit.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtClassLit: const-class should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtClassLit: invalid dex: {e:#}"));
}

/// `invokedynamic` (0xba) string concatenation (`StringConcatFactory.makeConcat[WithConstants]`)
/// — DESUGARED at SSA-build time to a `new StringBuilder; append…; toString()` chain of
/// ordinary synthesized SsaOps (NewInstance/Invoke/ConstString), so emit/regalloc need no
/// change. Parses the BootstrapMethods recipe (=arg, =constant, else literal)
/// and picks the right `StringBuilder.append` overload per arg JVM type (I/J/D/F/Z/C/
/// String/Object). `ArtConcat` runs on ART (tests/art/ArtConcat, stdout
/// `x=0,y=v0!|x=1,y=v1!|x=2,y=v2!|ab,c1truez3.5` — mixed int/boolean/char/double + nesting).
#[test]
fn ssa_string_concat_indy_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtConcat.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtConcat: string-concat indy should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtConcat: invalid dex: {e:#}"));
}

/// RANGE-FORM invokes (`invoke-*/range` 0x74-0x78, 3rc) — calls with >5 arg-registers
/// (or a high reg) that the 35c form can't encode. emit_invoke MARSHALS the args into a
/// CONSECUTIVE scratch block above the allocated set (move/from16 0x02, move-wide/from16
/// 0x05, move-object/from16 0x08) and emits invoke-*/range over it; the block stays
/// consecutive under args-high remap. Needed a new `Field::Word` (16-bit register field)
/// in regalloc for the move-src + range CCCC, + reg_fields/dex_insn_len for 0x02/0x05/
/// 0x08/0x74-0x78 (and fixed 0x05's mislabeled length). Drove kstdlib 83.5%→89.7%.
/// `ArtRange` runs on ART (tests/art/ArtRange, stdout `15,21,27,35,a1b2c3` — 6 int args,
/// wide long args, and object/String args).
#[test]
fn ssa_range_invoke_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtRange.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtRange: range invoke should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtRange: invalid dex: {e:#}"));
}

/// φ-moves on BRANCHING edges (previously bailed "needs edge-splitting"). Three cases:
/// a single-pred successor of a branching block (move at the successor's entry); a
/// CRITICAL fall-through edge (move emitted inline after the `if`, run only on fall-
/// through); a CRITICAL taken edge (move + goto in a trampoline block after the code,
/// taken branch redirected there). Coalescing still keeps interfering φ operands (e.g.
/// the clamp `x` vs loop counter `i`) in distinct registers, so the move is real, not a
/// clobber. Drove kstdlib 89.7%→90.6%. `ArtClamp` runs on ART (tests/art/ArtClamp,
/// stdout `35,3,85`).
#[test]
fn ssa_phi_move_branching_edges_now_dex() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtClamp.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtClamp: φ-move edges should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtClamp: invalid dex: {e:#}"));
}

/// PARTIALLY-DEAD CONST: `int r=0; if(c)r=…; return r` / `String s="no"; if(b)s="yes"`.
/// d8 SINKS the const into the surviving branch; we don't — we materialize it before the
/// `if` and flow it via its register / a φ-move on the merge edge (now that branching-edge
/// φ-moves are emitted). The const-sink BAIL was a byte-identity guard, dropped in
/// functional-correctness mode (drove kstdlib 90.6%→91.8%). `ArtConstSink` runs on ART
/// (tests/art/ArtConstSink → `6,0,yes,no,10,1`).
#[test]
fn ssa_partially_dead_const_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtConstSink.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtConstSink: should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtConstSink: invalid dex: {e:#}"));
}

/// NESTED loops (`for i { for j { s += i*j } }`) now DEX. The bail was a byte-identity
/// guard (d8 leaves an un-DCE'd dead const for the undefined-φ-entry; our DCE drops it —
/// smaller-but-correct). Dropped in functional-correctness mode (kstdlib 91.8%→92.1%).
/// `ArtNested` runs on ART (tests/art/ArtNested → `36,0,15,0`).
#[test]
fn ssa_nested_loops_now_dex() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtNested.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtNested: nested loops should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtNested: invalid dex: {e:#}"));
}

/// goto/16 BRANCH RELAXATION: a `goto` whose target is >±127 code-units away is widened
/// to goto/16 (0x29, 2 words) via a fixpoint relaxation pass (widening shifts all later
/// word-offsets — block_unit/fixups/pool_fixups/positions/throw_spans bumped, re-scan).
/// Previously bailed "goto offset N needs goto/16". `ArtGoto` has a big loop body forcing
/// a far back-edge goto; runs on ART (tests/art/ArtGoto → `0,552,10004`).
#[test]
fn ssa_goto16_relaxation_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtGoto.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtGoto: far goto should relax now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtGoto: invalid dex: {e:#}"));
}

/// catch-all / finally (`try{}finally{}`) — single-throwing-op trys. The catch-all goes
/// in the DEX try_item's catch_all_addr; the handler's rethrow now emits move-exception
/// because the `used` set counts TERMINATOR operands (the rethrow's only use of the
/// caught exception) — earlier this was missing and the rethrow threw an Undefined
/// register (ART VerifyError). `ArtFinally` runs BOTH paths on ART (tests/art/ArtFinally:
/// normal finally + exception-path finally+rethrow → `f7|fC`). (Multi-throwing-op trys
/// still bail on multi-predecessor/nested-region handlers — the next blocker.)
#[test]
fn ssa_try_finally_now_dexes() {
    for name in ["ArtFinally", "FinOnly"] {
        let cf = skotch_classfile::parse_class_file(&fixtures().join(format!("{name}.class"))).unwrap();
        let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
        let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("{name}: try-finally should dex now: {e:#}"));
        skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("{name}: invalid dex: {e:#}"));
    }
}

/// MULTI-PREDECESSOR HANDLERS: a try with >1 throwing op makes the catch handler
/// reachable from N throw points (N exceptional preds). Each snapshots the slot versions
/// into the handler-φs; a post-allocation check bails unless every handler-φ's operands
/// coalesced into ONE register (exceptional edges can't carry φ-moves — so they must
/// agree, else bail; never miscompile). Drove +38 classes across 5 libs (guava +21).
/// `ArtMultiCatch` (`try{a(sb);b(sb);}catch(e){...}`, 2 throwing calls) runs on ART
/// (tests/art/ArtMultiCatch → `abok,aC,abC` — normal + each throw point correct).
#[test]
fn ssa_multi_pred_handler_now_dexes() {
    let cf = skotch_classfile::parse_class_file(&fixtures().join("ArtMultiCatch.class")).unwrap();
    let opts = D8Options { min_api: 1, mode: Mode::Release, ..Default::default() };
    let dex = dex_classes(&[cf], &opts).unwrap_or_else(|e| panic!("ArtMultiCatch: multi-pred handler should dex now: {e:#}"));
    skotch_dex::validator::validate(&dex).unwrap_or_else(|e| panic!("ArtMultiCatch: invalid dex: {e:#}"));
}
