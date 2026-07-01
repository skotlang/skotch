data class Point2(val x: Int, val y: Int) {
    fun magnitude(): Double = kotlin.math.sqrt((x * x + y * y).toDouble())
}

fun main() {
    val p = Point2(3, 4)
    println("p=$p magnitude=${p.magnitude()}")
    println("doubled=${p.x * 2 + p.y * 2}")
    println("formatted=${"%.2f".format(p.magnitude())}")
}
