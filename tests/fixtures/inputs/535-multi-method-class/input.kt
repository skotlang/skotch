class Calculator {
    fun add(a: Int, b: Int): Int = a + b
    fun multiply(a: Int, b: Int): Int = a * b
    fun isPositive(n: Int): Boolean = n > 0
}

fun main() {
    val calc = Calculator()
    println(calc.add(3, 4))
    println(calc.multiply(5, 6))
    println(calc.isPositive(-1))
    println(calc.isPositive(42))
}
