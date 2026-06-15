// A dead store `sink = x + y` whose operands skotch const-propagates (x=0, y=2). `constant_fold`
// rewrites the identity-0 add to its operand, DCE drops the now-unused add from the block body,
// but it stays in `f.values` with both const operands sharing a register (legitimate hole reuse).
// The over-coalesce net used to flag this DEAD value — a false positive. `sink` must stay unused
// for the const operands to coalesce. (Kotlin keeps `var x=0; var y=2` as distinct loads.)
fun f(n: Int): Int {
    var acc = 0
    var sink = 0
    for (i in 0 until n) {
        var x = 0
        var y = 2
        sink = x + y
        x = i
        y = i + 1
        acc += x + y
    }
    return acc
}
fun main() {
    for (n in intArrayOf(0, 1, 6, 10)) println(f(n))
}
