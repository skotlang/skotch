fun main() {
    val m = mapOf("alice" to 30, "bob" to 25, "carol" to 28)
    val sorted = m.entries.sortedBy { it.value }
    for (e in sorted) println("${e.key}=${e.value}")
    println(m.keys.sorted())
    println(m.values.sorted())
    val avg = m.values.average()
    println(avg)
}
