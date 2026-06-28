fun classify(n: Int): String {
    if (n < 0) return "neg"
    if (n == 0) return "zero"
    val tier = when {
        n < 10 -> "small"
        n < 100 -> "medium"
        else -> "large"
    }
    return "$n/$tier"
}

fun main() {
    for (n in listOf(-5, 0, 7, 50, 999)) println(classify(n))
}
