fun main() {
    val xs = IntArray(5) { it * it }
    for (x in xs) print("$x ")
    println()
    println(xs.size)
    println(xs.sum())
    println(xs.average())
    println(xs.max())
    println(xs.min())
}
