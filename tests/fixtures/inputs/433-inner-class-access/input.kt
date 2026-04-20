class Matrix(val rows: Int, val cols: Int) {
    inner class Cell(val r: Int, val c: Int) {
        fun index(): Int = r * cols + c
    }
}

fun main() {
    val m = Matrix(3, 4)
    val cell = m.Cell(2, 3)
    println(cell.index())
}
