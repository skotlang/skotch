fun makePair(a: Int, b: Int): Pair<Int, Int> = Pair(a, b)
fun makeTriple(a: Int, b: Int, c: String): Triple<Int, Int, String> = Triple(a, b, c)

fun main() {
    val p = makePair(7, 11)
    println("${p.first} ${p.second}")
    val t = makeTriple(1, 2, "three")
    println("${t.first} ${t.second} ${t.third}")
    val (x, y) = 5 to 10
    println("$x,$y")
}
