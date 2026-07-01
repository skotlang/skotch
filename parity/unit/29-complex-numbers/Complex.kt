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
