fun sign(n: Int): Int = when {
    n > 0 -> 1
    n < 0 -> -1
    else -> 0
}

fun main() {
    println(sign(42))
    println(sign(-7))
    println(sign(0))
}
