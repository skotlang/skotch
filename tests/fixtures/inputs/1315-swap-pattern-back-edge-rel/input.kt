// `peephole_swap_pattern_with_branches` was casting `dst` to `usize`
// in its branch-offset adjustment guard. When the goto's recorded
// `rel` was correct PRE-splice but the splice removed bytes between
// goto's target and goto itself, the post-splice `dst = src + rel`
// became negative (because src had shifted earlier but rel wasn't
// yet adjusted). Casting that negative i32 to usize wrapped to
// `usize::MAX`, so the `dst <= start` check returned false and the
// peephole silently skipped adjusting the goto's offset. At
// runtime the JVM rejected the now-out-of-range goto with
// "Expecting a stackmap frame at branch target -5" / similar.
//
// The shape that hits it: a while-loop body that emits a swap-able
// astore/getstatic/aload pattern (typical of `println(stringConcat)`
// where MIR materializes the concat result via a spill before
// loading System.out).
//
// Fix: compare `src`/`dst` against `start`/`start+new_len` using i32
// throughout, dropping the usize cast that wrapped negative values.

fun work(n: Int): Int = n * 2

fun main() {
    var n = 1
    while (n <= 3) {
        println("n=" + n + " work=" + work(n))
        n = n + 1
    }
}
