data class Vec2(val x: Int, val y: Int) {
    operator fun plus(other: Vec2) = Vec2(x + other.x, y + other.y)
}

fun main() {
    val a = Vec2(1, 2)
    val b = Vec2(3, 4)
    println(a + b)
}
