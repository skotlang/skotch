class Counter {
    var count: Int = 0
    fun increment() { count++ }
    fun decrement() { count-- }
    fun reset() { count = 0 }
}

fun main() {
    val c = Counter()
    c.increment()
    c.increment()
    c.increment()
    println(c.count)
    c.decrement()
    println(c.count)
    c.reset()
    println(c.count)
}
