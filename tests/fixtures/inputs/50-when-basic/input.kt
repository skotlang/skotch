fun describe(n: Int): String = when (n) {
    1 -> "one"
    2 -> "two"
    3 -> "three"
    else -> "other"
}

fun main() {
    println(describe(1))
    println(describe(2))
    println(describe(99))
}
