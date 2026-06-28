fun main() {
    val xs = sequenceOf(1, 2, 3, 4, 5)
    val r = xs.map { it * 2 }.filter { it > 4 }.toList()
    println(r)
    println(r.sum())
    println((1..100).asSequence().take(3).toList())
}
