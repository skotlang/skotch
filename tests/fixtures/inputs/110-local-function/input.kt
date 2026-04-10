fun main() {
    fun factorial(n: Int): Int = if (n <= 1) 1 else n * factorial(n - 1)
    println(factorial(5))
    println(factorial(10))
}
