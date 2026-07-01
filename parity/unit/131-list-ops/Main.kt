fun main() {
    val xs = listOf(3, 1, 4, 1, 5, 9, 2, 6)
    println(xs.first())
    println(xs.last())
    println(xs.minOrNull())
    println(xs.maxOrNull())
    println(xs.average())
    println(xs.sorted())
    println(xs.reversed())
    println(xs.distinct())
    println(xs.indexOf(4))
}
