data class Color(val r: Int, val g: Int, val b: Int)

fun main() {
    val red = Color(255, 0, 0)
    val pink = red.copy(g = 128, b = 128)
    println(pink)
}
