fun classify(n: Int): String = when (n) {
    in 1..9 -> "single"
    in 10..99 -> "double"
    in 100..999 -> "triple"
    0 -> "zero"
    else -> if (n < 0) "neg" else "huge"
}

fun main() {
    for (n in listOf(0, 5, 42, 500, 9999, -7)) println("$n -> ${classify(n)}")
}
