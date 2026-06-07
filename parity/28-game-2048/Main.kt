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
