// Conway's Game of Life, represented as IMMUTABLE generations.
// Each `Generation` holds the grid as a flat IntArray (`r * width + c`)
// and produces the next generation via `step()` — returning a fresh
// Generation rather than mutating in place. The class has only `val`
// fields, which side-steps the var-field staleness pattern that bit
// examples 19 and 21.
//
// Sophistication step over example 22:
//   - class methods (rather than top-level fns) over an `IntArray`
//     backing store — exercises instance method dispatch against
//     `val`-only state without tripping the var-field-stale bug
//   - 2D neighbor counting with bounded edges (no wraparound)
//   - the standard B3/S23 transition rules

class Generation(val width: Int, val height: Int, val cells: IntArray) {
    fun cellAt(r: Int, c: Int): Int {
        return cells[r * width + c]
    }

    fun cellSafe(r: Int, c: Int): Int {
        if (r < 0) return 0
        if (r >= height) return 0
        if (c < 0) return 0
        if (c >= width) return 0
        return cells[r * width + c]
    }

    fun countNeighbors(r: Int, c: Int): Int {
        var count = 0
        count = count + cellSafe(r - 1, c - 1)
        count = count + cellSafe(r - 1, c)
        count = count + cellSafe(r - 1, c + 1)
        count = count + cellSafe(r, c - 1)
        count = count + cellSafe(r, c + 1)
        count = count + cellSafe(r + 1, c - 1)
        count = count + cellSafe(r + 1, c)
        count = count + cellSafe(r + 1, c + 1)
        return count
    }

    fun nextCell(r: Int, c: Int): Int {
        val n = countNeighbors(r, c)
        val alive = cellAt(r, c) == 1
        if (alive && (n == 2 || n == 3)) return 1
        if (!alive && n == 3) return 1
        return 0
    }

    fun step(): Generation {
        val next = IntArray(width * height)
        var r = 0
        while (r < height) {
            var c = 0
            while (c < width) {
                next[r * width + c] = nextCell(r, c)
                c++
            }
            r++
        }
        return Generation(width, height, next)
    }

    fun show(): String {
        val sb = StringBuilder()
        var r = 0
        while (r < height) {
            var c = 0
            while (c < width) {
                if (cellAt(r, c) == 1) {
                    sb.append('#')
                } else {
                    sb.append('.')
                }
                c++
            }
            sb.append('\n')
            r++
        }
        return sb.toString()
    }
}
