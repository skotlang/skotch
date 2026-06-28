fun main() {
    val xs = listOf(10, 20, 30, 40, 50)
    println(xs.take(2))
    println(xs.drop(2))
    println(xs.takeWhile { it < 30 })
    println(xs.dropWhile { it < 30 })
    println(xs.takeLast(2))
    println(xs.dropLast(2))
    println(xs.chunked(2))
}
