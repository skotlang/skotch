fun main() {
    val xs = listOf(1, 2, 3, 4, 5)
    println(xs.sum())
    println(xs.filter { it > 2 }.sum())
    val m = mapOf("a" to 1, "b" to 2)
    println(m["a"])
    println(setOf(1, 1, 2).size)
}
