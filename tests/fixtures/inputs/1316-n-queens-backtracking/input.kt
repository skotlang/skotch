// N-queens via straightforward column-per-row backtracking.
//
// `cols[r]` records the column where the queen on row `r` sits.
// `isSafe(cols, row, col)` checks against all prior placements
// (same column or same diagonal). The solver `place` recurses
// row-by-row, calling `count` whenever it places all N queens.
//
// `counter[0]` is used as a single-cell IntArray accumulator so
// `place` can mutate it from recursive calls without bringing in
// any class-with-var-field plumbing (which has been fragile in
// skotch's use-before-def analysis).
//
// Sophistication step over example 29:
//   - search-tree recursion with mutable shared state (counter)
//   - in-place column assignment with no explicit undo (each row
//     overwrites its slot on the next iteration)
//   - multiple early returns inside a constraint-check loop

fun isSafe(cols: IntArray, row: Int, col: Int): Boolean {
    var r = 0
    while (r < row) {
        if (cols[r] == col) return false
        val diff = col - cols[r]
        val rowDiff = row - r
        if (diff == rowDiff) return false
        if (diff == -rowDiff) return false
        r = r + 1
    }
    return true
}

fun place(cols: IntArray, n: Int, row: Int, counter: IntArray) {
    if (row == n) {
        counter[0] = counter[0] + 1
        return
    }
    var c = 0
    while (c < n) {
        if (isSafe(cols, row, c)) {
            cols[row] = c
            place(cols, n, row + 1, counter)
        }
        c = c + 1
    }
}

fun solveNQueens(n: Int): Int {
    val cols = IntArray(n)
    val counter = IntArray(1)
    place(cols, n, 0, counter)
    return counter[0]
}
// Solve N-queens for N = 1..9 and print the solution count for each.
// Expected counts (OEIS A000170): 1, 0, 0, 2, 10, 4, 40, 92, 352.

fun main() {
    var n = 1
    while (n <= 9) {
        println("N=" + n + ": " + solveNQueens(n) + " solutions")
        n = n + 1
    }
}
