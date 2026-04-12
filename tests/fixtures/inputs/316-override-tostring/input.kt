class Point(val x: Int, val y: Int) {
    override fun toString(): String = "($x, $y)"
}

fun main() {
    println(Point(3, 4))
    println(Point(0, 0))
}
