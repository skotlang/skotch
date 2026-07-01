data class Pos(val x: Int, val y: Int)

fun fromOrigin(p: Pos): Int {
    val (a, b) = p
    return a * a + b * b
}

fun main() {
    val p = Pos(3, 4)
    val (x, y) = p
    println("$x,$y")
    println(fromOrigin(p))
    println(fromOrigin(Pos(5, 12)))
}
