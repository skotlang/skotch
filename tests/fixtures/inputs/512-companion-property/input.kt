class Counter {
    companion object {
        var count: Int = 0
        fun increment() { count++ }
    }
}

fun main() {
    Counter.increment()
    Counter.increment()
    println(Counter.count)
}
