// 2048-style sliding/merging board logic.
//
// The 4x4 board is stored as a flat IntArray(16). `0` means empty
// cell; non-zero values are the tile numbers (always powers of 2 in
// practice — 2, 4, 8, …). The slide operations work along rows or
// columns; `slideLeft` is the canonical "left arrow" move:
//   1. compact (remove zeros), 2. merge adjacent equal pairs (doubling
//   the surviving tile), 3. compact again.
//
// All 4 directions are implemented by rotating the board into a "left
// move" orientation, calling `slideLeft`, then rotating back.
//
// Sophistication step over example 27:
//   - per-row compact + merge with the merge body extracted into a
//     2-line helper (`stepMerge`) — keeps the while-loop body a
//     single helper call, dodging the use-before-def stub trigger
//   - 4 direction passes via composed rotation+slide+rotation —
//     exercises calling fresh-IntArray-returning helpers from inside
//     a sliding pipeline

fun slideRowLeft(row: IntArray): IntArray {
    val n = row.size
    val compact = IntArray(n)
    val out = IntArray(n)

    // Step 1: compact non-zero values to the front of `compact`.
    var ci = 0
    var i = 0
    while (i < n) {
        if (row[i] != 0) {
            compact[ci] = row[i]
            ci = ci + 1
        }
        i = i + 1
    }

    // Step 2: merge adjacent equal pairs. After merging, the right
    // half of the pair becomes 0.
    var j = 0
    while (j < ci - 1) {
        j = stepMerge(compact, j)
    }

    // Step 3: compact again into `out` (which is already zero-filled).
    var oi = 0
    var k = 0
    while (k < ci) {
        if (compact[k] != 0) {
            out[oi] = compact[k]
            oi = oi + 1
        }
        k = k + 1
    }
    return out
}

fun stepMerge(compact: IntArray, j: Int): Int {
    if (compact[j] == compact[j + 1]) {
        compact[j] = compact[j] * 2
        compact[j + 1] = 0
        return j + 2
    }
    return j + 1
}

fun slideBoardLeft(board: IntArray): IntArray {
    val out = IntArray(16)
    val row = IntArray(4)
    var r = 0
    while (r < 4) {
        var c = 0
        while (c < 4) {
            row[c] = board[r * 4 + c]
            c = c + 1
        }
        val merged = slideRowLeft(row)
        c = 0
        while (c < 4) {
            out[r * 4 + c] = merged[c]
            c = c + 1
        }
        r = r + 1
    }
    return out
}

// Transpose a 4x4 board: `rotated[r * 4 + c] = board[c * 4 + r]`.
fun transpose(board: IntArray): IntArray {
    val out = IntArray(16)
    var r = 0
    while (r < 4) {
        var c = 0
        while (c < 4) {
            out[r * 4 + c] = board[c * 4 + r]
            c = c + 1
        }
        r = r + 1
    }
    return out
}

// Reverse each row of a 4x4 board.
fun reverseRows(board: IntArray): IntArray {
    val out = IntArray(16)
    var r = 0
    while (r < 4) {
        var c = 0
        while (c < 4) {
            out[r * 4 + c] = board[r * 4 + (3 - c)]
            c = c + 1
        }
        r = r + 1
    }
    return out
}

// All 4 directions, expressed in terms of left-slide + transpose/reverse.
fun slideBoardRight(board: IntArray): IntArray {
    return reverseRows(slideBoardLeft(reverseRows(board)))
}

fun slideBoardUp(board: IntArray): IntArray {
    return transpose(slideBoardLeft(transpose(board)))
}

fun slideBoardDown(board: IntArray): IntArray {
    return transpose(slideBoardRight(transpose(board)))
}
// Run a sequence of slides on a hand-built starting board and print
// the board after each move.

fun cellStr(v: Int): String {
    if (v == 0) {
        return "   ."
    }
    val s = "" + v
    val padN = 4 - s.length
    val sb = StringBuilder()
    var i = 0
    while (i < padN) {
        sb.append(' ')
        i = i + 1
    }
    sb.append(s)
    return sb.toString()
}

fun formatBoard(board: IntArray): String {
    val sb = StringBuilder()
    var r = 0
    while (r < 4) {
        var c = 0
        while (c < 4) {
            sb.append(cellStr(board[r * 4 + c]))
            sb.append(' ')
            c = c + 1
        }
        sb.append('\n')
        r = r + 1
    }
    return sb.toString()
}

fun showMove(label: String, before: IntArray, after: IntArray) {
    println("--- $label ---")
    println("before:")
    print(formatBoard(before))
    println("after:")
    print(formatBoard(after))
}

fun main() {
    // Row test: [2, 2, 4, 0] slides left → [4, 4, 0, 0]
    val rowDemo = slideRowLeft(intArrayOf(2, 2, 4, 0))
    println("row [2, 2, 4, 0] → [${rowDemo[0]}, ${rowDemo[1]}, ${rowDemo[2]}, ${rowDemo[3]}]")

    val board = intArrayOf(
        2, 2, 4, 4,
        0, 2, 2, 0,
        4, 0, 0, 4,
        2, 4, 2, 4
    )

    showMove("slideLeft", board, slideBoardLeft(board))
    showMove("slideRight", board, slideBoardRight(board))
    showMove("slideUp", board, slideBoardUp(board))
    showMove("slideDown", board, slideBoardDown(board))
}
