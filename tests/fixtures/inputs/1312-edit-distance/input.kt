// Levenshtein edit distance via bottom-up dynamic programming.
//
// The DP table `dp` is stored as a flat IntArray of size
// `(n+1) * (m+1)`, indexed as `dp[i * stride + j]` where
// `stride = m + 1`. This keeps the inner-loop assignment a single
// unconditional `dp[...] = cellValue(...)` (the per-cell logic is
// extracted into a helper so skotch's use-before-def analysis
// doesn't stub the loop body).
//
// Sophistication step over example 26:
//   - 2D DP on a flat IntArray with hand-computed strides
//   - String-character comparison inside a nested while-loop
//   - delegated per-cell helper for clean inner-loop body

fun min3(a: Int, b: Int, c: Int): Int {
    var x = a
    if (b < x) {
        x = b
    }
    if (c < x) {
        x = c
    }
    return x
}

fun cellValue(s1: String, s2: String, dp: IntArray, stride: Int, i: Int, j: Int): Int {
    if (s1[i - 1] == s2[j - 1]) {
        return dp[(i - 1) * stride + (j - 1)]
    }
    val deletion = dp[(i - 1) * stride + j] + 1
    val insertion = dp[i * stride + (j - 1)] + 1
    val substitution = dp[(i - 1) * stride + (j - 1)] + 1
    return min3(deletion, insertion, substitution)
}

fun editDistance(s1: String, s2: String): Int {
    val n = s1.length
    val m = s2.length
    val stride = m + 1
    val dp = IntArray((n + 1) * stride)

    // Row 0: dp[0][j] = j  (insert j chars to build s2[0..j))
    var j = 0
    while (j <= m) {
        dp[j] = j
        j = j + 1
    }
    // Column 0: dp[i][0] = i  (delete i chars from s1[0..i))
    var i = 0
    while (i <= n) {
        dp[i * stride] = i
        i = i + 1
    }

    // Fill the rest.
    i = 1
    while (i <= n) {
        var jj = 1
        while (jj <= m) {
            dp[i * stride + jj] = cellValue(s1, s2, dp, stride, i, jj)
            jj = jj + 1
        }
        i = i + 1
    }

    return dp[n * stride + m]
}
// Run editDistance on a handful of (s1, s2) pairs and print the result.

fun report(s1: String, s2: String) {
    val d = editDistance(s1, s2)
    println("d(\"$s1\", \"$s2\") = $d")
}

fun main() {
    report("", "")             // 0
    report("a", "")            // 1
    report("", "abc")          // 3
    report("abc", "abc")       // 0
    report("kitten", "sitting") // 3
    report("flaw", "lawn")     // 2
    report("intention", "execution") // 5
    report("Saturday", "Sunday")     // 3
    report("rosettacode", "raisethysword") // 8
}
