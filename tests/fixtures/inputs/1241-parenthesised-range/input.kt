fun main() {
    // Parenthesised range — must not be rejected by the parser. The
    // previous lookahead heuristic in parse_infix_call bailed when the
    // token two ahead of `..` was `)`, so any `(N..M)` failed with
    // "expected ), found DotDot".
    val r = (1..5)
    var sum = 0
    for (i in r) sum += i
    println(sum)

    // Same shape nested inside a call argument, exercising the
    // `)` lookahead from a different angle.
    var n = 0
    for (i in (10..12)) n += i
    println(n)
}
