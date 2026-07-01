data class Point(val x: Int, val y: Int, val label: String)

fun main() {
    val p = Point(1, 2, "a")
    val q = p.copy(y = 99)
    val r = p.copy(x = 7, label = "b")
    println(p)
    println(q)
    println(r)
}
