fun safeDivide(a: Int, b: Int): String {
    return try {
        val result = a / b
        "Result: $result"
    } catch (e: Exception) {
        "Error"
    }
}

fun main() {
    println(safeDivide(10, 2))
    println(safeDivide(10, 0))
}
