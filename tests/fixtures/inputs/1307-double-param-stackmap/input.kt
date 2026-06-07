// Double/Long param slots must be wide-aware in the initial-live set
// for block 0's SMT inheritance.
//
// Pre-fix: a fn like `fun f(a: Double, b: Double, n: Int)` had params
// occupying JVM slots 0-1 (a), 2-3 (b), and 4 (n). The live-slot
// dataflow initialized block 0's `inherited` set as
// `s < initial_locals_count` where `initial_locals_count =
// func.params.len()` — so only slots 0..3 were marked alive. The
// later SMT frame at the loop's back-edge then computed
// `merged[4] = false`, write_slot_verif emitted Top for slot 4, and
// the verifier rejected the first `iload 4` of `n` with
// "Bad local variable type" / "Type top ... not assignable to integer".
//
// Fix: use `initial_param_slots` (already wide-aware — Double/Long
// params count for 2) instead of `initial_locals_count` for the live
// set seeding (and for the `num_locals` fallback when no slots are
// live in a frame).

fun escapeIter(x0: Double, y0: Double, maxIter: Int): Int {
    var x = 0.0
    var y = 0.0
    var i = 0
    while (i < maxIter && x * x + y * y < 4.0) {
        val xNew = x * x - y * y + x0
        y = 2.0 * x * y + y0
        x = xNew
        i = i + 1
    }
    return i
}

fun main() {
    println(escapeIter(0.0, 0.0, 50))      // never escapes → returns 50
    println(escapeIter(2.0, 2.0, 50))      // escapes immediately
    println(escapeIter(-1.0, 0.0, 50))     // -1 is on the real axis,
                                            // converges to fixed point
}
