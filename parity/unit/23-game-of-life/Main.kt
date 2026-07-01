// Run a glider through 4 generations on a 10x10 grid. The glider
// pattern translates diagonally one cell every 4 steps; this output
// shows the first 5 generations side-by-side.

fun gliderInitial(): IntArray {
    // .#........     (1,1) placed via row 0..., col 0...
    // ..#.......
    // ###.......
    // ..........
    // ...
    val cells = IntArray(100)
    // glider at top-left
    cells[0 * 10 + 1] = 1
    cells[1 * 10 + 2] = 1
    cells[2 * 10 + 0] = 1
    cells[2 * 10 + 1] = 1
    cells[2 * 10 + 2] = 1
    return cells
}

fun main() {
    var gen = Generation(10, 10, gliderInitial())
    var i = 0
    while (i <= 4) {
        println("--- Generation $i ---")
        print(gen.show())
        gen = gen.step()
        i = i + 1
    }
}
