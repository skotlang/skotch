data class Pair2(val a: Int, val b: Int)

fun pick(c: Boolean): Pair2 {
    val p = if (c) Pair2(1, 2) else Pair2(99, 100)
    return p
}

fun main() {
    println(pick(true))
    println(pick(false))
}
