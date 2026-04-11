fun main() {
    val x = -5
    val result = when (x) {
        -5 -> "minus five"
        0 -> "zero"
        5 -> "five"
        else -> "other"
    }
    println(result)
}
