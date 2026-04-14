fun main() {
    var count = 0
    val inc = { count = count + 1 }
    inc()
    inc()
    inc()
    println(count)
}
