class Point(val x: Int, val y: Int) {
    override fun toString(): String = "($x, $y)"
}

fun main() {
    val p = Point(3, 4)
    println(p)
}
