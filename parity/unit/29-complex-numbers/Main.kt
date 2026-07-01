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
