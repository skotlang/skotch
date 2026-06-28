fun main() {
    val xs = listOf(1, 2, 3, 4, 5)
    println(xs.any { it > 3 })
    println(xs.any { it > 100 })
    println(xs.all { it > 0 })
    println(xs.all { it > 3 })
    println(xs.none { it > 100 })
    println(xs.count { it % 2 == 0 })
}
