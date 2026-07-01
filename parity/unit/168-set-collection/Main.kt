fun main() {
    val s = mutableSetOf<String>()
    s.add("a")
    s.add("b")
    s.add("a")
    println(s.size)
    s.remove("a")
    println(s)
    println(s.contains("b"))
    val r = setOf(1, 2, 3, 2, 1)
    println(r.size)
}
