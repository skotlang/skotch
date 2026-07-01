fun main() {
    val xs = listOf("apple", "banana", "avocado", "blueberry", "cherry")
    val byFirst = xs.groupBy { it.first() }
    for ((k, v) in byFirst.toSortedMap()) println("$k: $v")
    println(xs.groupBy { it.length }.toSortedMap())
}
