data class Point(val x: Int, val y: Int)

fun main() {
    val p1 = Point(1, 2)
    val p2 = Point(1, 2)
    val p3 = Point(3, 4)
    println(p1 == p2)
    println(p1 == p3)
    println(p1.hashCode() == p2.hashCode())
    val p4 = p1.copy()
    println(p4)
    println(p4 == p1)
}
