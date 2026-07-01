fun main() {
    val xs = listOf("a", "bb", "ccc")
    val it = xs.iterator()
    while (it.hasNext()) {
        val s = it.next()
        println("$s:${s.length}")
    }
}
