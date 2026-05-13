suspend fun describe(n: Int): String = when (n) {
    0 -> "zero"
    1 -> "one"
    else -> "many"
}
