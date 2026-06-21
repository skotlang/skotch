//! d8's register-allocation register space.
//!
//! r8/d8's `LinearScanRegisterAllocator` allocates values in an "allocated
//! register space" where the incoming arguments occupy the FIRST
//! `num_arg_registers` registers (`0..num_arg`), and every other value is
//! allocated above them — reusing an argument register only once that argument
//! is dead. After allocation it remaps "allocated" numbers to "real" DEX
//! registers via `realRegisterNumberFromAllocated`, which moves the arguments to
//! the TOP registers and packs the locals at the bottom:
//!
//! ```text
//!   allocated < num_arg   →  real = max_reg - (num_arg - allocated - 1)   (args, high)
//!   allocated >= num_arg  →  real = allocated - num_arg                   (locals, low)
//! ```
//!
//! Our dexer already allocates in exactly this allocated space (arguments at
//! `[0, num_arg)`, temporaries reusing dead-argument registers via liveness).
//! Applying the same remap reproduces d8's args-high placement. Note the remap
//! is the IDENTITY whenever `registers_used == num_arg` (no extra locals) or
//! `num_arg == 0` (no arguments) — i.e. for every method without register
//! pressure — so it changes nothing for the no-pressure cases and only fixes the
//! pressure cases that previously had to bail.

/// Maps one allocated register to its real DEX register.
pub fn remap_register(allocated: u16, num_arg: u16, registers_used: u16) -> u16 {
    debug_assert!(registers_used >= 1);
    let max_reg = registers_used - 1;
    if allocated < num_arg {
        max_reg - (num_arg - allocated - 1)
    } else {
        allocated - num_arg
    }
}

/// A register-bearing field within a DEX instruction word stream.
#[derive(Clone, Copy)]
enum Field {
    /// Low nibble of word `w` high byte: bits 8..12.
    NibbleA {
        w: usize,
    },
    /// High nibble of word `w` high byte: bits 12..16.
    NibbleB {
        w: usize,
    },
    /// Whole high byte of word `w`: bits 8..16 (an 8-bit register).
    ByteAA {
        w: usize,
    },
    /// Low byte of word `w`: bits 0..8.
    ByteLo {
        w: usize,
    },
    /// High byte of word `w`: bits 8..16.
    ByteHi {
        w: usize,
    },
    /// Low nibble of word `w` low byte: bits 0..4 (35c arg nibble vC..vF).
    NibbleLo0 {
        w: usize,
    },
    NibbleLo1 {
        w: usize,
    }, // bits 4..8
    /// The whole 16-bit word `w` as a register (move/from16 src BBBB, invoke/range CCCC).
    Word {
        w: usize,
    },
}

fn get_field(insns: &[u16], base: usize, f: Field) -> u16 {
    match f {
        Field::NibbleA { w } => (insns[base + w] >> 8) & 0xf,
        Field::NibbleB { w } => (insns[base + w] >> 12) & 0xf,
        Field::ByteAA { w } => (insns[base + w] >> 8) & 0xff,
        Field::ByteLo { w } => insns[base + w] & 0xff,
        Field::ByteHi { w } => (insns[base + w] >> 8) & 0xff,
        Field::NibbleLo0 { w } => insns[base + w] & 0xf,
        Field::NibbleLo1 { w } => (insns[base + w] >> 4) & 0xf,
        Field::Word { w } => insns[base + w],
    }
}

fn set_field(insns: &mut [u16], base: usize, f: Field, v: u16) {
    match f {
        Field::NibbleA { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0x0f00) | ((v & 0xf) << 8);
        }
        Field::NibbleB { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0xf000) | ((v & 0xf) << 12);
        }
        Field::ByteAA { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0xff00) | ((v & 0xff) << 8);
        }
        Field::ByteLo { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0x00ff) | (v & 0xff);
        }
        Field::ByteHi { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0xff00) | ((v & 0xff) << 8);
        }
        Field::NibbleLo0 { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0x000f) | (v & 0xf);
        }
        Field::NibbleLo1 { w } => {
            let x = &mut insns[base + w];
            *x = (*x & !0x00f0) | ((v & 0xf) << 4);
        }
        Field::Word { w } => {
            insns[base + w] = v;
        }
    }
}

/// Length (in 16-bit code units) of the DEX instruction at `insns[base]`, for
/// the opcodes our dexer emits.
fn dex_insn_len(op: u8) -> usize {
    match op {
        // 1-word: move-result(0x0a-0x0c), move-exception(0x0d), return-void(0x0e),
        //         return(0x0f-0x11), const/4(0x12), move(0x01), move-wide(0x04),
        //         move-object(0x07), throw(0x27), goto(0x28), array-length(0x21),
        //         monitor-enter(0x1d)/monitor-exit(0x1e), unops(0x7b-0x8f), 2addr(0xb0-0xcf)
        //         (NOTE: 0x05 move-wide/from16 is 22x = 2 words — NOT here.)
        0x0a..=0x12
        | 0x01
        | 0x04
        | 0x07
        | 0x1d
        | 0x1e
        | 0x21
        | 0x27
        | 0x28
        | 0x7b..=0x8f
        | 0xb0..=0xcf => 1,
        // 3-word: const(0x14), const-wide/32(0x17), goto/32(0x2a), invoke 35c(0x6e-0x72),
        //         invoke/range 3rc(0x74-0x78)
        0x14 | 0x17 | 0x2a | 0x6e..=0x72 | 0x74..=0x78 => 3,
        // 5-word: const-wide(0x18)
        0x18 => 5,
        // everything else our dexer emits is 2 words
        _ => 2,
    }
}

/// Register fields of the instruction at `insns[base]` (opcode `op`). For the
/// 35c invoke the caller handles the variable argument nibbles separately.
fn reg_fields(op: u8) -> &'static [Field] {
    match op {
        // 11x: move-result / move-exception(0x0d) / return / throw(0x27) /
        //      monitor-enter(0x1d) / monitor-exit(0x1e) — AA (objectref) in word0 high byte
        0x0a..=0x0d | 0x0f..=0x11 | 0x1d | 0x1e | 0x27 => &[Field::ByteAA { w: 0 }],
        0x0e => &[], // return-void
        // 11n const/4 — A nibble
        0x12 => &[Field::NibbleA { w: 0 }],
        // 12x: move(0x01) / move-wide(0x04) / move-object(0x07) / array-length(0x21) /
        //      unops / 2addr binops — A, B nibbles. (0x05 move-wide/from16 is 22x — below.)
        0x01 | 0x04 | 0x07 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => {
            &[Field::NibbleA { w: 0 }, Field::NibbleB { w: 0 }]
        }
        // 21*: const/16, const/high16, const-wide/16, const-wide/high16,
        //      const-string, const-class, check-cast, new-instance, if-testz, sget/sput — AA only
        0x13 | 0x15 | 0x16 | 0x19 | 0x1a | 0x1c | 0x1f | 0x22 | 0x38..=0x3d | 0x60..=0x6d => {
            &[Field::ByteAA { w: 0 }]
        }
        // 31i / 31t / 51l: const, const-wide/32, const-wide — AA only
        0x14 | 0x17 | 0x18 => &[Field::ByteAA { w: 0 }],
        // 22c (iget/iput, new-array, instance-of) / 22t (if-test) / 22s (lit16)
        0x52..=0x5f | 0x32..=0x37 | 0xd0..=0xd7 | 0x23 | 0x20 => {
            &[Field::NibbleA { w: 0 }, Field::NibbleB { w: 0 }]
        }
        // 22b (lit8) — AA in word0 + BB in word1 low byte. Covers the arith lit8 ops
        // (add/rsub/mul/div/rem/and/or/xor-int/lit8, 0xd8–0xdf) AND the shift lit8 ops
        // (shl/shr/ushr-int/lit8, 0xe0–0xe2) — all same format; CC (the literal) in
        // word1 high byte is not a register and is left untouched.
        0xd8..=0xe2 => &[Field::ByteAA { w: 0 }, Field::ByteLo { w: 1 }],
        // 23x (3addr binops int/long/float/double, aget*/aput*, cmp*) —
        //   AA word0, BB word1 lo, CC hi
        0x90..=0xaf | 0x44..=0x51 | 0x2d..=0x31 => &[
            Field::ByteAA { w: 0 },
            Field::ByteLo { w: 1 },
            Field::ByteHi { w: 1 },
        ],
        // 22x move/from16 + move-wide/from16 + move-object/from16:
        //   AA dest (word0 high byte) + BBBB src (whole word1).
        0x02 | 0x05 | 0x08 => &[Field::ByteAA { w: 0 }, Field::Word { w: 1 }],
        // 3rc invoke-*/range: AA=arg count (word0, NOT a register), BBBB=method (word1),
        //   CCCC=first register of the consecutive arg block (whole word2).
        0x74..=0x78 => &[Field::Word { w: 2 }],
        _ => &[],
    }
}

/// Remaps every register operand in `insns` from allocated space to real DEX
/// registers (args-high), in place.
/// The largest register a field can ENCODE: 15 for a 4-bit nibble, 255 for an 8-bit byte, 65535
/// for a 16-bit word. Used to detect when a remapped register no longer fits its instruction form
/// (which would otherwise be silently truncated by `set_field` — a miscompile).
fn field_max(f: Field) -> u16 {
    match f {
        Field::NibbleA { .. }
        | Field::NibbleB { .. }
        | Field::NibbleLo0 { .. }
        | Field::NibbleLo1 { .. } => 15,
        Field::ByteAA { .. } | Field::ByteLo { .. } | Field::ByteHi { .. } => 255,
        Field::Word { .. } => u16::MAX,
    }
}

/// Remap allocated register numbers to real DEX numbers (args to the top). A method may legitimately
/// use >16 registers: the wide instruction forms (8/16-bit fields) encode high registers fine, and
/// only the 4-bit nibble forms (move 12x, if 22t, invoke 35c args, …) cap at 15. So instead of a
/// blanket >16 bail, we check each field as we remap and FAIL (never truncate) when a register
/// doesn't fit — that operand genuinely needs spilling, which this allocator doesn't do.
pub fn remap_insns(insns: &mut [u16], num_arg: u16, registers_used: u16) -> anyhow::Result<()> {
    remap_insns_skip(
        insns,
        num_arg,
        registers_used,
        &std::collections::HashSet::new(),
    )
}

/// A nibble field at `insns[word]` bits `[shift, shift+4)` whose register was emitted PRE-REMAPPED
/// (already its final args-high value) by `nibw` for a high-allocated local — `remap_insns` must
/// leave it alone. NibbleA = bits 8..12, NibbleB = bits 12..16; other field kinds are never spilled.
fn field_skip_key(base: usize, f: Field) -> Option<(usize, u16)> {
    match f {
        Field::NibbleA { w } => Some((base + w, 8)),
        Field::NibbleB { w } => Some((base + w, 12)),
        _ => None,
    }
}

pub fn remap_insns_skip(
    insns: &mut [u16],
    num_arg: u16,
    registers_used: u16,
    skip: &std::collections::HashSet<(usize, u16)>,
) -> anyhow::Result<()> {
    // Identity remap — nothing to do (and avoids touching anything).
    let identity = num_arg == 0 || registers_used == num_arg;
    let map = |r: u16| {
        if identity {
            r
        } else {
            remap_register(r, num_arg, registers_used)
        }
    };

    let mut base = 0;
    while base < insns.len() {
        let op = (insns[base] & 0xff) as u8;
        let len = dex_insn_len(op);
        // Fixed register fields.
        for &f in reg_fields(op) {
            // A pre-remapped high-local nibble (emitted as its final value by `nibw`) is left as-is.
            if field_skip_key(base, f).is_some_and(|k| skip.contains(&k)) {
                continue;
            }
            let r = map(get_field(insns, base, f));
            if r > field_max(f) {
                anyhow::bail!(
                    "regalloc: register v{r} does not fit the instruction form for op {op:#04x} (max v{}) — needs spilling",
                    field_max(f)
                );
            }
            set_field(insns, base, f, r);
        }
        // 35c invoke: variable argument registers (all 4-bit nibbles, cap 15).
        if (0x6e..=0x72).contains(&op) {
            let count = (insns[base] >> 12) & 0xf;
            // vG (5th reg) is word0 bits 8..12; vC..vF are word2 nibbles.
            if count == 5 {
                let g = map(get_field(insns, base, Field::NibbleA { w: 0 }));
                if g > 15 {
                    anyhow::bail!(
                        "regalloc: invoke 5th-arg register v{g} > 15 — needs range form/spilling"
                    );
                }
                set_field(insns, base, Field::NibbleA { w: 0 }, g);
            }
            let arg_fields = [
                Field::NibbleLo0 { w: 2 },
                Field::NibbleLo1 { w: 2 },
                Field::NibbleA { w: 2 },
                Field::NibbleB { w: 2 },
            ];
            for k in 0..(count.min(4) as usize) {
                let r = map(get_field(insns, base, arg_fields[k]));
                if r > 15 {
                    anyhow::bail!(
                        "regalloc: invoke arg register v{r} > 15 — needs range form/spilling"
                    );
                }
                set_field(insns, base, arg_fields[k], r);
            }
        }
        base += len;
    }
    Ok(())
}

/// The highest register operand referenced anywhere in `insns` (in allocated space, i.e.
/// before the args-high remap), or `None` if there are no register operands. A NO_REG
/// (u16::MAX) operand emitted as a register byte/nibble surfaces here as an out-of-range
/// value — build_dex uses this as a safety net to BAIL instead of emitting garbage like
/// `aget v2, v3, v253` (the rematerialization NO_REG bug).
pub fn max_register_used(insns: &[u16]) -> Option<u16> {
    let mut max: Option<u16> = None;
    let mut note = |r: u16| max = Some(max.map_or(r, |m| m.max(r)));
    let mut base = 0;
    while base < insns.len() {
        let op = (insns[base] & 0xff) as u8;
        for &f in reg_fields(op) {
            note(get_field(insns, base, f));
        }
        if (0x6e..=0x72).contains(&op) {
            let count = (insns[base] >> 12) & 0xf;
            if count == 5 {
                note(get_field(insns, base, Field::NibbleA { w: 0 }));
            }
            let arg_fields = [
                Field::NibbleLo0 { w: 2 },
                Field::NibbleLo1 { w: 2 },
                Field::NibbleA { w: 2 },
                Field::NibbleB { w: 2 },
            ];
            for k in 0..(count.min(4) as usize) {
                note(get_field(insns, base, arg_fields[k]));
            }
        }
        base += dex_insn_len(op);
    }
    max
}

#[cfg(test)]
mod opcode_table_audit {
    //! Every DEX opcode the SSA emit path can produce MUST be covered by `reg_fields`
    //! (so the args-high remap rewrites its register operands) AND by `dex_insn_len`
    //! (so the remap walks instruction boundaries without desyncing). A gap is a SILENT
    //! MISCOMPILE — exactly the bug `shl/shr/ushr-int/lit8` (0xe0–0xe2) hit when it was
    //! emitted but fell outside `reg_fields`' 22b range (0xd8..=0xdf): its registers
    //! were never remapped while everyone else's were. This cross-checks each
    //! opcode-mapping helper's FULL output against both tables by instruction format,
    //! so a new emitted opcode added to a mapper but forgotten in the tables fails here
    //! instead of silently corrupting a register.
    use super::{dex_insn_len, reg_fields};
    use crate::bootstrap::{
        binop_2addr_op, binop_3addr_op, cmp_op, cond_branch_dex_op, lit_ops, shift_lit8_op,
    };

    fn check(op: u16, regs: usize, len: usize, what: &str) {
        let o = op as u8;
        assert_eq!(
            reg_fields(o).len(),
            regs,
            "{what}: op {o:#04x} reg_fields field count"
        );
        assert_eq!(dex_insn_len(o), len, "{what}: op {o:#04x} dex_insn_len");
    }

    #[test]
    fn binop_3addr_ops_covered() {
        // 23x: vAA, vBB, vCC — 3 register fields, 2 words.
        for jvm in 0u8..=0xff {
            if let Ok(op) = binop_3addr_op(jvm) {
                check(op, 3, 2, "binop_3addr (23x)");
            }
        }
    }

    #[test]
    fn binop_2addr_ops_covered() {
        // 12x: vA, vB — 2 nibble register fields, 1 word.
        for jvm in 0u8..=0xff {
            if let Some(op) = binop_2addr_op(jvm) {
                check(op, 2, 1, "binop_2addr (12x)");
            }
        }
    }

    #[test]
    fn lit_ops_covered() {
        // 22s lit16 and 22b lit8 — 2 register fields (vA/vB or vAA/vBB) + literal, 2 words.
        for jvm in 0u8..=0xff {
            if let Some((op8, op16)) = lit_ops(jvm) {
                check(op8, 2, 2, "lit8 (22b)");
                check(op16, 2, 2, "lit16 (22s)");
            }
        }
    }

    #[test]
    fn shift_lit8_ops_covered() {
        // 22b shift-by-const — 2 register fields, 2 words. Regression guard for the
        // 0xe0–0xe2 remap-range gap.
        for jvm in 0u8..=0xff {
            if let Some(op) = shift_lit8_op(jvm) {
                check(op, 2, 2, "shift-lit8 (22b)");
            }
        }
    }

    #[test]
    fn cmp_ops_covered() {
        // 23x cmpl/cmpg-float, cmpl/cmpg-double, cmp-long — 3 register fields, 2 words.
        for jvm in [0x94u8, 0x95, 0x96, 0x97, 0x98] {
            let (op, _) = cmp_op(jvm);
            check(op, 3, 2, "cmp (23x)");
        }
    }

    #[test]
    fn cond_branch_ops_covered() {
        // 22t two-operand if-test (2 fields) vs 21t if-testz (1 field); both 2 words.
        for jvm in 0u8..=0xff {
            if let Some((op, two)) = cond_branch_dex_op(jvm) {
                check(op, if two { 2 } else { 1 }, 2, "cond-branch");
            }
        }
    }

    #[test]
    fn move_ops_covered() {
        // The 12x moves emit_phi_moves / wide-const-sharing emit directly (not via a
        // mapper): move(0x01), move-wide(0x04), move-object(0x07) — 2 register fields,
        // 1 word. (Regression guard for the move-wide 0x04 remap-table gap.)
        for op in [0x01u16, 0x04, 0x07] {
            check(op, 2, 1, "move (12x)");
        }
    }

    #[test]
    fn unop_and_conversion_ops_covered() {
        // neg-int/long/float/double + the i2x conversion ops emit_value produces
        // (12x: vA, vB — 2 register fields, 1 word).
        for op in [0x7bu16, 0x7d, 0x7f, 0x80]
            .into_iter()
            .chain(0x81u16..=0x8f)
        {
            check(op, 2, 1, "unop/convert (12x)");
        }
    }
}
