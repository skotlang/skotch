fun <T : Comparable<T>> maxOf(a: T, b: T): T = if (a > b) a else b

fun main() {
    println(maxOf(3, 7))
    println(maxOf(10, 2))
}
