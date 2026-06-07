// Run several pattern + input pairs through the parser+matcher and
// print whether each matched.

fun describe(pattern: String, input: String): String {
    val re = parseRegex(pattern)
    val ok = matches(re, input)
    val result = if (ok) "match" else "no match"
    return "/$pattern/ vs \"$input\" => $result"
}

fun main() {
    println(describe("a", "a"))
    println(describe("a", "b"))
    println(describe("ab", "ab"))
    println(describe("ab", "a"))
    println(describe("a|b", "a"))
    println(describe("a|b", "b"))
    println(describe("a|b", "c"))
    println(describe("a*", ""))
    println(describe("a*", "aaa"))
    println(describe("a*", "aab"))
    println(describe("a*b", "b"))
    println(describe("a*b", "aaab"))
    println(describe("a*b", "aaa"))
    println(describe(".", "x"))
    println(describe(".*", "anything goes"))
    println(describe("a.*b", "axxxb"))
    println(describe("(ab)*", "ababab"))
    println(describe("(ab)*c", "ababc"))
    println(describe("(a|b)*c", "aabbabc"))
    println(describe("(a|b)*c", "abxc"))
}
