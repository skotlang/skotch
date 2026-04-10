fun <T> singletonList(item: T): List<T> = listOf(item)

fun <T : Comparable<T>> maxOf(a: T, b: T): T = if (a > b) a else b

fun main() {
    println(singletonList(42))
    println(maxOf(3, 7))
    println(maxOf("apple", "banana"))
}
