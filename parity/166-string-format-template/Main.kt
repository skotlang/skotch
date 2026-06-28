data class Coord(val x: Int, val y: Int)

fun main() {
    val p = Coord(3, 4)
    println("at $p")
    println("x=${p.x} y=${p.y}")
    val xs = listOf(1, 2, 3)
    println("size=${xs.size} first=${xs[0]}")
    val name = "world"
    println("greeting: hello, $name!")
}
