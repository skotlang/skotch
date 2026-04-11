class Pair(val first: Int, val second: Int)

fun makePair(a: Int, b: Int): Pair = Pair(a, b)

fun main() {
    val p = makePair(10, 20)
    println(p.first)
    println(p.second)
}
