fun classify(x: Int): String = when {
    x > 10 -> "big"
    x > 3 -> "medium"
    else -> "small"
}

fun main() {
    println(classify(15))
    println(classify(5))
    println(classify(1))
}
