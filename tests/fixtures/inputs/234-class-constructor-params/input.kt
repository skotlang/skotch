class Point(val x: Int, val y: Int) {
    fun distFromOriginSquared(): Int = x * x + y * y
}

fun main() {
    val p = Point(3, 4)
    println(p.distFromOriginSquared())
    println(p.x)
    println(p.y)
}
