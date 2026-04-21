class Grid(val rows: Int, val cols: Int) {
    private val data = IntArray(rows * cols)
    operator fun get(r: Int, c: Int): Int = data[r * cols + c]
    operator fun set(r: Int, c: Int, value: Int) { data[r * cols + c] = value }
}

fun main() {
    val g = Grid(3, 3)
    g[1, 1] = 42
    println(g[1, 1])
}
