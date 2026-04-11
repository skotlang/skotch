fun double(x: Int): Int = x * 2
fun isPositive(n: Int): Boolean = n > 0
fun negate(x: Int): Int = -x

fun main() {
    println(double(21))
    println(isPositive(5))
    println(isPositive(-3))
    println(negate(7))
}
