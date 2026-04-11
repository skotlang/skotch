fun classify(x: Int): String = when {
    x < 0 -> "negative"
    x == 0 -> "zero"
    x < 100 -> "small"
    else -> "big"
}

fun main() {
    println(classify(-5))
    println(classify(0))
    println(classify(42))
    println(classify(999))
}
