class Counter2 {
    var count: Int = 0
    fun increment() { count++ }
    fun add(n: Int) { count += n }
    fun reset() { count = 0 }
    fun show(): String = "count=$count"
}

fun main() {
    val c = Counter2()
    c.increment()
    c.increment()
    c.add(10)
    println(c.show())
    c.reset()
    println(c.show())
}
