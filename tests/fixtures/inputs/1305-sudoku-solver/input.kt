// Sudoku solver using straightforward recursive backtracking.
//
// Board is a flat IntArray(81) — `board[row * 9 + col]`. Empty cells
// are 0; filled cells are 1..9. The solver tries each candidate that
// satisfies the row/column/3x3-box constraints, recurses, and undoes
// the placement if the branch fails.
//
// Sophistication step over example 21:
//   - 3 simultaneous constraints per cell (row, column, 3x3 box)
//   - mutation through the recursive search (backtrack on failure)
//   - explicit `solved: Boolean` flag returned by `solve` so the
//     caller can detect unsolvable inputs
//   - pretty-printer that draws 9x9 grid with `-` for empties and
//     `|`/`+` separators every 3 cells

fun cellAt(board: IntArray, row: Int, col: Int): Int {
    return board[row * 9 + col]
}

fun setCell(board: IntArray, row: Int, col: Int, value: Int) {
    board[row * 9 + col] = value
}

fun isCandidateLegal(board: IntArray, row: Int, col: Int, candidate: Int): Boolean {
    // Row check: any cell in this row already hold the candidate?
    var c = 0
    while (c < 9) {
        if (cellAt(board, row, c) == candidate) return false
        c++
    }
    // Column check.
    var r = 0
    while (r < 9) {
        if (cellAt(board, r, col) == candidate) return false
        r++
    }
    // 3x3 box check.
    val boxRow = (row / 3) * 3
    val boxCol = (col / 3) * 3
    var br = 0
    while (br < 3) {
        var bc = 0
        while (bc < 3) {
            if (cellAt(board, boxRow + br, boxCol + bc) == candidate) return false
            bc++
        }
        br++
    }
    return true
}

// Find the first empty cell, return its row*9+col index, or -1 if
// no empty cells remain.
fun nextEmpty(board: IntArray): Int {
    var i = 0
    while (i < 81) {
        if (board[i] == 0) return i
        i++
    }
    return -1
}

// Returns true if the board is fully solved (recursive backtracking).
// Mutates `board` in place — caller can read out the solution after.
fun solve(board: IntArray): Boolean {
    val idx = nextEmpty(board)
    if (idx == -1) return true
    val row = idx / 9
    val col = idx - row * 9
    var candidate = 1
    while (candidate <= 9) {
        if (isCandidateLegal(board, row, col, candidate)) {
            setCell(board, row, col, candidate)
            if (solve(board)) return true
            setCell(board, row, col, 0)
        }
        candidate++
    }
    return false
}

// Pretty-print a 9x9 grid. Cells separated by " "; every 3 cols a
// "| " divider; every 3 rows a "------+-------+------" divider line.
fun formatBoard(board: IntArray): String {
    val sb = StringBuilder()
    var r = 0
    while (r < 9) {
        if (r > 0 && r % 3 == 0) {
            sb.append("------+-------+------\n")
        }
        var c = 0
        while (c < 9) {
            if (c > 0 && c % 3 == 0) {
                sb.append("| ")
            }
            val v = cellAt(board, r, c)
            if (v == 0) {
                sb.append(". ")
            } else {
                sb.append(v)
                sb.append(' ')
            }
            c++
        }
        sb.append('\n')
        r++
    }
    return sb.toString()
}
// Two-puzzle demo. Each puzzle is encoded as an 81-cell IntArray
// (row-major, 0 = empty). We print the puzzle, solve it in place,
// then print the solution.

fun easyPuzzle(): IntArray {
    return intArrayOf(
        5, 3, 0,  0, 7, 0,  0, 0, 0,
        6, 0, 0,  1, 9, 5,  0, 0, 0,
        0, 9, 8,  0, 0, 0,  0, 6, 0,

        8, 0, 0,  0, 6, 0,  0, 0, 3,
        4, 0, 0,  8, 0, 3,  0, 0, 1,
        7, 0, 0,  0, 2, 0,  0, 0, 6,

        0, 6, 0,  0, 0, 0,  2, 8, 0,
        0, 0, 0,  4, 1, 9,  0, 0, 5,
        0, 0, 0,  0, 8, 0,  0, 7, 9
    )
}

fun mediumPuzzle(): IntArray {
    return intArrayOf(
        0, 0, 0,  2, 6, 0,  7, 0, 1,
        6, 8, 0,  0, 7, 0,  0, 9, 0,
        1, 9, 0,  0, 0, 4,  5, 0, 0,

        8, 2, 0,  1, 0, 0,  0, 4, 0,
        0, 0, 4,  6, 0, 2,  9, 0, 0,
        0, 5, 0,  0, 0, 3,  0, 2, 8,

        0, 0, 9,  3, 0, 0,  0, 7, 4,
        0, 4, 0,  0, 5, 0,  0, 3, 6,
        7, 0, 3,  0, 1, 8,  0, 0, 0
    )
}

fun runPuzzle(label: String, board: IntArray) {
    println(label)
    println("Puzzle:")
    print(formatBoard(board))
    val ok = solve(board)
    if (ok) {
        println("Solution:")
        print(formatBoard(board))
    } else {
        println("No solution.")
    }
}

fun main() {
    runPuzzle("=== Easy ===", easyPuzzle())
    runPuzzle("=== Medium ===", mediumPuzzle())
}
