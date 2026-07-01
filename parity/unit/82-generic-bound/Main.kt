fun <T : Comparable<T>> maxOf3(a: T, b: T, c: T): T {
    val ab = if (a > b) a else b
    return if (ab > c) ab else c
}

fun main() {
    println(maxOf3(1, 7, 3))
    println(maxOf3("apple", "banana", "cherry"))
    println(maxOf3(1.5, 2.5, 0.5))
}
