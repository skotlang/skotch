// ASCII renderer for the Mandelbrot set.
//
// For each pixel `(px, py)` we map to a complex point `c = (x0, y0)`
// in the rectangle `x ∈ [-2.5, 1.0]`, `y ∈ [-1.0, 1.0]`, then iterate
// the standard escape-time formula
//
//     z_{n+1} = z_n^2 + c   with z_0 = 0
//
// counting how many iterations elapse before |z|^2 ≥ 4 (or hitting
// `maxIter` if it never escapes — these are the "inside" points,
// rendered as `#`). The shading ramp matches a coarse logarithmic
// bucketing so the boundary fringe shows depth.
//
// Sophistication step over example 23:
//   - all-Double arithmetic (`x * x + y * y`, etc.) in a tight loop
//   - integer-to-Double mixed expressions (`3.5 * px / width`)
//   - returns a Char from a small lookup function (no when-arms)
//   - escape detection compares `(x*x + y*y) < 4.0` in the while
//     condition (Double `<` against a literal)

fun escapeIterations(x0: Double, y0: Double, maxIter: Int): Int {
    var x = 0.0
    var y = 0.0
    var i = 0
    while (i < maxIter && x * x + y * y < 4.0) {
        val xNew = x * x - y * y + x0
        y = 2.0 * x * y + y0
        x = xNew
        i = i + 1
    }
    return i
}

fun shadeFor(iter: Int, maxIter: Int): Char {
    if (iter >= maxIter) return '#'
    if (iter * 2 >= maxIter) return '*'
    if (iter * 4 >= maxIter) return '+'
    if (iter * 8 >= maxIter) return '.'
    return ' '
}

fun renderMandelbrot(width: Int, height: Int, maxIter: Int): String {
    val sb = StringBuilder()
    var py = 0
    while (py < height) {
        var px = 0
        while (px < width) {
            val x0 = -2.5 + 3.5 * px / width
            val y0 = -1.0 + 2.0 * py / height
            val iter = escapeIterations(x0, y0, maxIter)
            sb.append(shadeFor(iter, maxIter))
            px = px + 1
        }
        sb.append('\n')
        py = py + 1
    }
    return sb.toString()
}
// Render the Mandelbrot set at 60x20 with 50 escape iterations.

fun main() {
    print(renderMandelbrot(60, 20, 50))
}
