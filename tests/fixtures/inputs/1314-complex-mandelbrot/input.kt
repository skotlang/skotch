// Complex numbers with operator overloading. Each operator returns a
// fresh `Complex` — the class is fully immutable (val fields only),
// so calls like `z * z + c` build a small temporary tree before the
// final binding.
//
// Sophistication step over example 28:
//   - `operator fun plus / minus / times` on a class — exercises
//     Kotlin's operator-overload dispatch with multiple methods on
//     the same receiver type
//   - method chaining `z * z + c` → desugars to `z.times(z).plus(c)`,
//     a 2-call chain returning fresh instances
//   - downstream `mandelbrotEscape(c, maxIter)` uses Complex methods
//     inside a tight Double-comparison loop

class Complex(val re: Double, val im: Double) {
    operator fun plus(other: Complex): Complex {
        return Complex(re + other.re, im + other.im)
    }

    operator fun minus(other: Complex): Complex {
        return Complex(re - other.re, im - other.im)
    }

    operator fun times(other: Complex): Complex {
        val newRe = re * other.re - im * other.im
        val newIm = re * other.im + im * other.re
        return Complex(newRe, newIm)
    }

    fun magnitudeSquared(): Double {
        return re * re + im * im
    }

    fun show(): String {
        return "(" + re + " + " + im + "i)"
    }
}
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
// Small arithmetic demo + Mandelbrot via Complex.

fun main() {
    val a = Complex(1.0, 2.0)
    val b = Complex(3.0, 4.0)

    println("a = " + a.show())
    println("b = " + b.show())
    println("a + b = " + (a + b).show())
    println("a - b = " + (a - b).show())
    println("a * b = " + (a * b).show())
    println("|a|² = " + a.magnitudeSquared())

    println("---")
    print(renderViaComplex(60, 20, 50))
}
