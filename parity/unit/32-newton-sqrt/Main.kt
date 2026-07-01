// Newton's method for square roots, plus a small Fibonacci-ratio
// convergence test that uses the computed `sqrt(5)` to print the
// golden ratio φ = (1 + √5) / 2 ≈ 1.6180339887….
//
// Sophistication step over example 31:
//   - Double convergence loop with epsilon comparison
//   - Double absolute value (no Math.abs dependency, since stdlib
//     reflection is uneven across skotch's intrinsic table)
//   - mixed Int/Double arithmetic in a while loop (Fibonacci pair
//     update + DOUBLE ratio computation each iteration)

fun absD(x: Double): Double {
    if (x < 0.0) return -x
    return x
}

fun sqrt(x: Double): Double {
    if (x == 0.0) return 0.0
    var guess = x / 2.0
    var i = 0
    while (i < 50) {
        val next = (guess + x / guess) / 2.0
        if (absD(next - guess) < 1.0e-10) {
            return next
        }
        guess = next
        i = i + 1
    }
    return guess
}
// Print √0..√10 (computed via Newton's method), then watch the ratio
// of consecutive Fibonacci numbers converge to the golden ratio
// φ = (1 + √5) / 2.

fun main() {
    println("--- Newton's method for sqrt ---")
    var n = 0
    while (n <= 10) {
        val s = sqrt(n.toDouble())
        println("sqrt($n) = $s")
        n = n + 1
    }

    println("--- Fibonacci ratio → golden ratio ---")
    val phi = (1.0 + sqrt(5.0)) / 2.0
    println("target phi = $phi")
    var a = 1
    var b = 1
    var i = 0
    while (i < 15) {
        val ratio = b.toDouble() / a.toDouble()
        println("F($i) ratio = $ratio")
        val next = a + b
        a = b
        b = next
        i = i + 1
    }
}
