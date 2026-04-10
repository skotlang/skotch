// TODO: `when` with a subject expression and exhaustiveness check.
fun describe(n: Int): String = when (n) {
    in 0..9 -> "small"
    in 10..99 -> "medium"
    else -> "large"
}
