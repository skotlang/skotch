fun main() {
    val xs = listOf("a", "bb", "ccc")
    val it = xs.iterator()
    while (it.hasNext()) {
        val s = it.next()
        println("$s:${s.length}")
    }
    val ys = listOf(10, 20, 30)
    for ((i, v) in ys.withIndex()) println("$i=$v")
}
