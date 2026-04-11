class Counter(val start: Int) {
    fun value(): Int = start
}

fun main() {
    val c = Counter(42)
    println(c.value())
    println(Counter(100).value())
}
