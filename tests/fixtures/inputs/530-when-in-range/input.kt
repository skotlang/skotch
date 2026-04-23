fun classify(n: Int): String = when (n) {
    in 1..10 -> "small"
    in 11..100 -> "medium"
    else -> "big"
}

fun main() {
    println(classify(5))
    println(classify(50))
    println(classify(500))
}
