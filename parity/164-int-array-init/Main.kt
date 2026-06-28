fun main() {
    val a = intArrayOf(1, 2, 3, 4, 5)
    println(a.size)
    println(a.sum())
    val b = IntArray(5)
    for (i in 0 until b.size) b[i] = i * i
    println(b.toList())
    val c = IntArray(3) { it + 100 }
    println(c.toList())
}
