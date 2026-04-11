fun describe(n: Int): String {
    return when (n) {
        0 -> "zero"
        1 -> "one"
        else -> "many"
    }
}

fun main() {
    println(describe(1))
}
