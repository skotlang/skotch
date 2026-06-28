fun fib(n: Int): Int = if (n <= 1) n else fib(n - 1) + fib(n - 2)
fun fact(n: Int): Long = if (n <= 1) 1L else n * fact(n - 1)

fun main() {
    for (i in 0..10) print("${fib(i)} ")
    println()
    println(fact(10))
    println(fact(15))
}
