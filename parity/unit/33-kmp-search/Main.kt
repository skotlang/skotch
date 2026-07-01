// Knuth-Morris-Pratt string-matching algorithm.
//
// KMP precomputes a "failure function" `f[i]` = length of the longest
// proper prefix of `pattern[0..i]` that is also a suffix. When a
// mismatch happens at pattern[q] vs text[i], we jump q to f[q-1]
// instead of restarting — guaranteeing linear-time matching.
//
// Sophistication step over example 32:
//   - failure-function preprocessing reuses the SAME helper as the
//     scan loop (`advanceQ`), self-referentially reading the partial
//     `f` array as it's filled in
//   - all-IntArray scratch buffer for collected match positions with
//     a manual count (no MutableList<Int> generic-erasure indirection)
//   - char comparisons inside nested while loops (the helper hides
//     the inner while so the outer loops stay flat)
//
// Single-file to dodge iter-33's cross-file peephole asymmetry.

fun advanceQ(pattern: String, q: Int, c: Char, f: IntArray): Int {
    var k = q
    while (k > 0 && pattern[k] != c) {
        k = f[k - 1]
    }
    if (pattern[k] == c) {
        return k + 1
    }
    return k
}

fun computeFailure(pattern: String): IntArray {
    val f = IntArray(pattern.length)
    var k = 0
    var i = 1
    while (i < pattern.length) {
        k = advanceQ(pattern, k, pattern[i], f)
        f[i] = k
        i = i + 1
    }
    return f
}

fun kmpSearch(text: String, pattern: String): IntArray {
    if (pattern.length == 0) return IntArray(0)
    val f = computeFailure(pattern)
    val scratch = IntArray(text.length)
    var matchCount = 0
    var q = 0
    var i = 0
    while (i < text.length) {
        q = advanceQ(pattern, q, text[i], f)
        if (q == pattern.length) {
            scratch[matchCount] = i - q + 1
            matchCount = matchCount + 1
            q = f[q - 1]
        }
        i = i + 1
    }
    val out = IntArray(matchCount)
    var k = 0
    while (k < matchCount) {
        out[k] = scratch[k]
        k = k + 1
    }
    return out
}

fun reportMatches(text: String, pattern: String) {
    val matches = kmpSearch(text, pattern)
    print("\"$pattern\" in \"$text\" =>")
    if (matches.size == 0) {
        println(" (no matches)")
        return
    }
    var i = 0
    while (i < matches.size) {
        print(" ")
        print(matches[i])
        i = i + 1
    }
    println()
}

fun main() {
    reportMatches("ABABCABABCD", "ABABCD")
    reportMatches("AAAAAAAA", "AAA")
    reportMatches("abracadabra", "abra")
    reportMatches("mississippi", "ss")
    reportMatches("mississippi", "issi")
    reportMatches("the quick brown fox", "fox")
    reportMatches("hello world", "xyz")
    reportMatches("aaaaab", "aaab")
    reportMatches("AAAAAA", "AAAAA")
    reportMatches("", "anything")
}
