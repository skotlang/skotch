fun squaredDist(x1: Int, y1: Int, x2: Int, y2: Int): Int {
    val dx = x2 - x1
    val dy = y2 - y1
    return dx * dx + dy * dy
}

fun main() {
    println(squaredDist(0, 0, 3, 4))
    println(squaredDist(1, 1, 4, 5))
    println(squaredDist(0, 0, 0, 0))
}
