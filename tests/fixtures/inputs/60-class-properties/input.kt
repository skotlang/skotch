class Counter {
    var count: Int = 0
    fun increment() { count++ }
}

fun main() {
    val c = Counter()
    c.increment()
    c.increment()
    c.increment()
    println(c.count)
}
