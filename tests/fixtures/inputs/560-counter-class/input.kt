class Counter(var count: Int) {
    fun increment() { count++ }
    fun decrement() { count-- }
    fun reset() { count = 0 }
    fun value(): Int = count
}
fun main() {
    val c = Counter(0)
    c.increment()
    c.increment()
    c.increment()
    println(c.value())
    c.decrement()
    println(c.value())
    c.reset()
    println(c.value())
}
