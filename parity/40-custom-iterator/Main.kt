// Drives both custom-iterator examples.
//
// Range2 yields Ints — exercises the primitive `next(): Int` path.
// Words yields Strings — exercises the reference `next(): String`
// path. Both confirm the for-loop user-iterator dispatch handles
// cross-file iterator classes (Range.kt and Walker.kt are separate
// from Main.kt).

fun main() {
    // ── Range2: numeric iteration ────────────────────────────────
    for (i in Range2(1, 5)) {
        println(i)
    }

    var sum = 0
    for (i in Range2(10, 15)) {
        sum = sum + i
    }
    println("range sum: $sum")          // 10+11+12+13+14+15 = 75

    // Empty range — start > end means hasNext() is false immediately.
    for (i in Range2(5, 3)) {
        println("never: $i")
    }
    println("empty range handled")

    // ── Words: string iteration via custom iterator ──────────────
    for (w in Words("hello world foo bar")) {
        println(w)
    }

    var concat = ""
    for (w in Words("a b c d e")) {
        concat = concat + w + "|"
    }
    println("concat: $concat")          // "a|b|c|d|e|"
}
