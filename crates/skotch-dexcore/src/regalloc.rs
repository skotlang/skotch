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
    NibbleA { w: usize },
    /// High nibble of word `w` high byte: bits 12..16.
    NibbleB { w: usize },
    /// Whole high byte of word `w`: bits 8..16 (an 8-bit register).
    ByteAA { w: usize },
    /// Low byte of word `w`: bits 0..8.
    ByteLo { w: usize },
    /// High byte of word `w`: bits 8..16.
    ByteHi { w: usize },
    /// Low nibble of word `w` low byte: bits 0..4 (35c arg nibble vC..vF).
    NibbleLo0 { w: usize },
    NibbleLo1 { w: usize }, // bits 4..8
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
    }
}

fn set_field(insns: &mut [u16], base: usize, f: Field, v: u16) {
    match f {
        Field::NibbleA { w } => { let x = &mut insns[base + w]; *x = (*x & !0x0f00) | ((v & 0xf) << 8); }
        Field::NibbleB { w } => { let x = &mut insns[base + w]; *x = (*x & !0xf000) | ((v & 0xf) << 12); }
        Field::ByteAA { w } => { let x = &mut insns[base + w]; *x = (*x & !0xff00) | ((v & 0xff) << 8); }
        Field::ByteLo { w } => { let x = &mut insns[base + w]; *x = (*x & !0x00ff) | (v & 0xff); }
        Field::ByteHi { w } => { let x = &mut insns[base + w]; *x = (*x & !0xff00) | ((v & 0xff) << 8); }
        Field::NibbleLo0 { w } => { let x = &mut insns[base + w]; *x = (*x & !0x000f) | (v & 0xf); }
        Field::NibbleLo1 { w } => { let x = &mut insns[base + w]; *x = (*x & !0x00f0) | ((v & 0xf) << 4); }
    }
}

/// Length (in 16-bit code units) of the DEX instruction at `insns[base]`, for
/// the opcodes our dexer emits.
fn dex_insn_len(op: u8) -> usize {
    match op {
        // 1-word: move-result(0x0a-0x0c), return-void(0x0e), return(0x0f-0x11),
        //         const/4(0x12), move(0x01), array-length(0x21), unops(0x7b-0x8f),
        //         2addr(0xb0-0xcf)
        0x0a..=0x0c | 0x0e..=0x12 | 0x01 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => 1,
        // 3-word: const(0x14), const-wide/32(0x17), invoke 35c(0x6e-0x72)
        0x14 | 0x17 | 0x6e..=0x72 => 3,
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
        // 11x: move-result / return — AA in word0 high byte
        0x0a..=0x0c | 0x0f..=0x11 => &[Field::ByteAA { w: 0 }],
        0x0e => &[], // return-void
        // 11n const/4 — A nibble
        0x12 => &[Field::NibbleA { w: 0 }],
        // 12x: move / array-length / unops / 2addr binops — A, B nibbles
        0x01 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => &[Field::NibbleA { w: 0 }, Field::NibbleB { w: 0 }],
        // 21*: const/16, const/high16, const-wide/16, const-wide/high16,
        //      const-string, check-cast, if-testz, sget/sput — AA only
        0x13 | 0x15 | 0x16 | 0x19 | 0x1a | 0x1f | 0x38..=0x3d | 0x60..=0x6d => {
            &[Field::ByteAA { w: 0 }]
        }
        // 31i / 31t / 51l: const, const-wide/32, const-wide — AA only
        0x14 | 0x17 | 0x18 => &[Field::ByteAA { w: 0 }],
        // 22c (iget/iput, new-array, instance-of) / 22t (if-test) / 22s (lit16)
        0x52..=0x5f | 0x32..=0x37 | 0xd0..=0xd7 | 0x23 | 0x20 => {
            &[Field::NibbleA { w: 0 }, Field::NibbleB { w: 0 }]
        }
        // 22b (lit8) — AA in word0 + BB in word1 low byte
        0xd8..=0xdf => &[Field::ByteAA { w: 0 }, Field::ByteLo { w: 1 }],
        // 23x (3addr binops, aget*/aput*) — AA word0, BB word1 low, CC word1 high
        0x90..=0x9a | 0x44..=0x51 => {
            &[Field::ByteAA { w: 0 }, Field::ByteLo { w: 1 }, Field::ByteHi { w: 1 }]
        }
        _ => &[],
    }
}

/// Remaps every register operand in `insns` from allocated space to real DEX
/// registers (args-high), in place.
pub fn remap_insns(insns: &mut [u16], num_arg: u16, registers_used: u16) {
    // Identity remap — nothing to do (and avoids touching anything).
    let identity = num_arg == 0 || registers_used == num_arg;
    let map = |r: u16| if identity { r } else { remap_register(r, num_arg, registers_used) };

    let mut base = 0;
    while base < insns.len() {
        let op = (insns[base] & 0xff) as u8;
        let len = dex_insn_len(op);
        // Fixed register fields.
        for &f in reg_fields(op) {
            let r = get_field(insns, base, f);
            set_field(insns, base, f, map(r));
        }
        // 35c invoke: variable argument registers.
        if (0x6e..=0x72).contains(&op) {
            let count = (insns[base] >> 12) & 0xf;
            // vG (5th reg) is word0 bits 8..12; vC..vF are word2 nibbles.
            if count == 5 {
                let g = get_field(insns, base, Field::NibbleA { w: 0 });
                set_field(insns, base, Field::NibbleA { w: 0 }, map(g));
            }
            let arg_fields = [
                Field::NibbleLo0 { w: 2 },
                Field::NibbleLo1 { w: 2 },
                Field::NibbleA { w: 2 },
                Field::NibbleB { w: 2 },
            ];
            for k in 0..(count.min(4) as usize) {
                let r = get_field(insns, base, arg_fields[k]);
                set_field(insns, base, arg_fields[k], map(r));
            }
        }
        base += len;
    }
}
