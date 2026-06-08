// `reverse` uses a local recursive function that captures BOTH the
// outer `s` (val) and `builder` (val pointing at a mutable
// StringBuilder). The recursion is the interesting bit: the
// recursive `helper(i - 1)` call has to re-forward the captured
// `builder` and `s` to itself, not just pass the user's `i - 1`.
//
// Captures: 2 (one ref, one ref-to-mutable). Explicit param: 1 (Int).
// Recursion: tail-position self-call.

fun reverse(s: String): String {
    val builder = StringBuilder()
    fun helper(i: Int) {
        if (i < 0) return
        builder.append(s[i])
        helper(i - 1)
    }
    helper(s.length - 1)
    return builder.toString()
}

// `sumDigits` has a different capture shape: the recursive `step`
// takes BOTH the remaining number and the accumulator as explicit
// params (so no var-capture / Ref-boxing is needed), but captures
// the outer `multiplier` Int val. Tests that an Int capture +
// multiple Long args coexist correctly in the local fn signature.
fun sumDigits(n: Int, multiplier: Int): Long {
    fun step(k: Int, acc: Long): Long {
        if (k == 0) return acc
        return step(k / 10, acc + (k % 10).toLong() * multiplier)
    }
    return step(n, 0L)
}
