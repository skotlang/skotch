fun process(n: Int): Int {
    var result = 0
    try {
        result = if (n < 0) throw IllegalArgumentException("neg") else n * 2
    } catch (e: IllegalArgumentException) {
        result = -1
    } finally {
        result += 100
    }
    return result
}

fun main() {
    println(process(5))
    println(process(-1))
    println(process(10))
}
