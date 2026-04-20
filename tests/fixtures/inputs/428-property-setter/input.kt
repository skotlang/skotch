class Counter {
    var count: Int = 0
        set(value) {
            if (value >= 0) {
                field = value
            }
        }
}

fun main() {
    val c = Counter()
    c.count = 5
    println(c.count)
    c.count = -1
    println(c.count)
}
