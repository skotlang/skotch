fun name(n: Int): String {
    val s = when (n) {
        1 -> "one"
        2 -> "two"
        else -> "many"
    }
    return s
}

fun main() {
    println(name(1))
    println(name(2))
    println(name(99))
}
