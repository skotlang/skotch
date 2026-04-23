fun divide(a: Int, b: Int): Int {
    if (b == 0) throw IllegalArgumentException("Division by zero")
    return a / b
}

fun main() {
    println(divide(10, 2))
    try {
        divide(10, 0)
    } catch (e: Exception) {
        println(e.toString())
    }
}
