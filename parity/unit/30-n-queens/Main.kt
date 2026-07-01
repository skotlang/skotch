// Solve N-queens for N = 1..9 and print the solution count for each.
// Expected counts (OEIS A000170): 1, 0, 0, 2, 10, 4, 40, 92, 352.

fun main() {
    var n = 1
    while (n <= 9) {
        println("N=" + n + ": " + solveNQueens(n) + " solutions")
        n = n + 1
    }
}
