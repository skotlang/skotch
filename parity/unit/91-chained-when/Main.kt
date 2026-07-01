fun bracket(n: Int): String {
    val tier = when {
        n < 0 -> "neg"
        n == 0 -> "zero"
        n < 10 -> "small"
        n < 100 -> "medium"
        else -> "large"
    }
    return "$n→$tier"
}

fun main() {
    for (n in listOf(-5, 0, 7, 42, 999)) println(bracket(n))
}
