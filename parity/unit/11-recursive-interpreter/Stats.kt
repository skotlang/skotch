// A separate "visitor"-style walker that counts nums and ops without
// adding a new method to Expr. Uses `when (e) is X` smart casts to
// read subclass-specific fields. The shared IntArray accumulator is
// mutated in place during the recursive walk.
fun collectStats(e: Expr, acc: IntArray) {
    when (e) {
        is Num -> {
            acc[0] = acc[0] + 1
        }
        is Add -> {
            acc[1] = acc[1] + 1
            collectStats(e.l, acc)
            collectStats(e.r, acc)
        }
        is Mul -> {
            acc[1] = acc[1] + 1
            collectStats(e.l, acc)
            collectStats(e.r, acc)
        }
        is Neg -> {
            acc[1] = acc[1] + 1
            collectStats(e.x, acc)
        }
    }
}

fun statsOf(e: Expr): String {
    val acc = IntArray(2)
    collectStats(e, acc)
    return "nums=${acc[0]} ops=${acc[1]}"
}
