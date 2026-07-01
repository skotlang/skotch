// Algorithms over Matrix that exercise multi-arg index in both
// read and write positions, cross-file. Demonstrates that the
// parser desugaring works inside loops and nested expressions.

fun identity(n: Int): Matrix {
    val m = Matrix(n, n)
    var i = 0
    while (i < n) {
        m[i, i] = 1
        i = i + 1
    }
    return m
}

fun transpose(src: Matrix): Matrix {
    val out = Matrix(src.cols, src.rows)
    var r = 0
    while (r < src.rows) {
        var c = 0
        while (c < src.cols) {
            out[c, r] = src[r, c]
            c = c + 1
        }
        r = r + 1
    }
    return out
}

// Trace = sum of diagonal entries — proves both row+col indexing
// and that get(i, i) dispatches correctly to the multi-arg operator.
fun trace(m: Matrix): Int {
    val n = if (m.rows < m.cols) m.rows else m.cols
    var sum = 0
    var i = 0
    while (i < n) {
        sum = sum + m[i, i]
        i = i + 1
    }
    return sum
}
