fun describe(n: Int): String = when (n) {
    in 0..9 -> "small"
    in 10..99 -> "medium"
    else -> "large"
}

fun main() {
    println(describe(5))
    println(describe(50))
    println(describe(500))
}
