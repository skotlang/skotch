private fun compute(s: Int): Int {
    if (s % 7 == 0) throw RuntimeException("d7")
    return s * 2
}

// The catch body `throw if (n>0) A else B` is a stack-spanning merge (two checkcast-Throwable
// branches converge on one athrow) INSIDE a handler body — the operand-stack-underflow trigger.
fun g(n: Int): Int {
    var s = 0
    for (i in 0 until n) s += i
    return try {
        compute(s)
    } catch (e: Exception) {
        throw if (n > 0) RuntimeException("R") else IllegalStateException("I")
    }
}

fun main() {
    for (n in intArrayOf(5, 6, 7, 0)) {
        try { println(g(n)) } catch (e: Throwable) { println("threw " + e.javaClass.simpleName) }
    }
}
