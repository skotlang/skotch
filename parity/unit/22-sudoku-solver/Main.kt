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
