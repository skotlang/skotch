data class Point(val x: Int, val y: Int) {
    fun translate(dx: Int, dy: Int): Point = Point(x + dx, y + dy)
}

class Counter(private var n: Int = 0) {
    fun bump(): Int { n++; return n }
    fun value(): Int = n
}
