fun main() {
    val xs = listOf(3, 1, 4, 1, 5, 9, 2, 6)
    println(xs.sortedDescending())
    println(xs.minByOrNull { -it })
    println(xs.maxByOrNull { -it })
    println(xs.windowed(3))
    println(xs.zipWithNext().take(3))
    println(xs.partition { it < 4 })
}
