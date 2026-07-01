tailrec fun factorial(n: Int, acc: Long = 1L): Long =
    if (n <= 1) acc else factorial(n - 1, acc * n)

tailrec fun sumTo(n: Int, acc: Long = 0L): Long =
    if (n == 0) acc else sumTo(n - 1, acc + n)

fun main() {
    println(factorial(10))
    println(sumTo(100))
}
