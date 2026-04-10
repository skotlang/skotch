// TODO: data class. Requires synthesizing equals/hashCode/toString/copy
// and component<N>() accessors.
data class Point(val x: Int, val y: Int)

fun main() {
    val p = Point(1, 2)
    println(p)
}
