fun isPositive(n: Int): Boolean = n > 0

fun main() {
    val numbers = listOf(-2, -1, 0, 1, 2)
    val positives = numbers.filter(::isPositive)
    println(positives)
}
