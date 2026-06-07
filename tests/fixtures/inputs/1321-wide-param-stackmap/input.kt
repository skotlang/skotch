// Regression: a function with descriptor `(JIJJ)J` (4 params, the
// first/third/fourth being Long, so 7 JVM slots) plus an early-return
// branch used to emit a StackMapTable that initialized the implicit
// entry frame from `func.params.len()` (= 4) instead of the wide-aware
// slot count (= 7). The init loop bound `s < initial_locals_count`
// stopped after only 3 entries (Long, Int, Long), missing the 4th
// param (Long). When the post-branch frame was a same-locals delta,
// the JVM verifier reconstructed entry → +1 Long = 5 entries, but the
// actual locals state was 4 entries (the 4 params). Result:
//   `VerifyError: Inconsistent stackmap frames at branch target N:
//    Type top (current frame, locals[7]) is not assignable to long`
//
// Fix: `initial_locals_count` now uses `initial_param_slots` (already
// wide-aware via the param-walking sum at line ~4170).
//
// Originally surfaced by parity/35-tailrec-arithmetic's `powMod`.

fun powMod(base: Long, exp: Int, mod: Long, acc: Long): Long {
    if (exp == 0) return acc
    return powMod(base, exp - 1, mod, (acc * base) % mod)
}

fun main() {
    println(powMod(2L, 30, 1_000_000_007L, 1L))
}
