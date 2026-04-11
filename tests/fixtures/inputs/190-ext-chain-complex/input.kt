fun Int.isEven(): Boolean = this % 2 == 0
fun Int.isOdd(): Boolean = !this.isEven()
fun Int.abs(): Int = if (this < 0) -this else this

fun main() {
    println((-7).abs())
    println(4.isEven())
    println(5.isOdd())
    println((-4).abs().isEven())
}
