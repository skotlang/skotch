fun Int.double(): Int = this * 2
fun Int.addOne(): Int = this + 1

fun main() {
    val result = 5.double().addOne()
    println(result)
    println(3.addOne().double())
}
