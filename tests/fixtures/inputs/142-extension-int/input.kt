fun Int.isEven(): Boolean = this % 2 == 0
fun Int.square(): Int = this * this
fun Int.abs(): Int = if (this < 0) -this else this

fun main() {
    println(4.isEven())
    println(7.isEven())
    println(5.square())
    println((-3).abs())
}
