data class Point3(val x: Int, val y: Int, val z: Int)

fun main() {
    val a = Point3(1, 2, 3)
    val b = Point3(1, 2, 3)
    val c = Point3(4, 5, 6)
    println(a)
    println(a == b)
    println(a == c)
    println(a.hashCode() == b.hashCode())
    val d = a.copy(z = 99)
    println(d)
    val (x, y, z) = a
    println("$x,$y,$z")
}
