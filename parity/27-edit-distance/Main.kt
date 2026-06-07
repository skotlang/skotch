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
