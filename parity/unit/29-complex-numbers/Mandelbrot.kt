// Mandelbrot escape via Complex arithmetic. The classic iteration
// `z = z*z + c` is now a 2-call chain through Complex's overloaded
// operators (`z.times(z).plus(c)`).

fun shadeFor(iter: Int, maxIter: Int): Char {
    if (iter >= maxIter) return '#'
    if (iter * 2 >= maxIter) return '*'
    if (iter * 4 >= maxIter) return '+'
    if (iter * 8 >= maxIter) return '.'
    return ' '
}

fun mandelbrotEscape(c: Complex, maxIter: Int): Int {
    var z = Complex(0.0, 0.0)
    var i = 0
    while (i < maxIter && z.magnitudeSquared() < 4.0) {
        z = z * z + c
        i = i + 1
    }
    return i
}

fun renderViaComplex(width: Int, height: Int, maxIter: Int): String {
    val sb = StringBuilder()
    var py = 0
    while (py < height) {
        var px = 0
        while (px < width) {
            val x0 = -2.5 + 3.5 * px / width
            val y0 = -1.0 + 2.0 * py / height
            val iter = mandelbrotEscape(Complex(x0, y0), maxIter)
            sb.append(shadeFor(iter, maxIter))
            px = px + 1
        }
        sb.append('\n')
        py = py + 1
    }
    return sb.toString()
}
