fun Int.isPositive(): Boolean = this > 0
fun Int.negate(): Int = -this
fun Int.doubled(): Int = this * 2

fun main() {
    println(5.isPositive())
    println((-3).isPositive())
    println(5.negate())
    println(7.doubled())
}
