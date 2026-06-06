// Regression: `Iterable<T>.fold(R, (R, T) -> R)` must type the
// lambda's two parameters as `R` (from the initial accumulator) and
// `T` (from the iterable's element type). Before the fix, both
// params were typed `Any`, the body's `acc + s.area()` was lowered
// as `Int+Int` with `Integer.intValue()` unbox calls, and runtime
// threw `ClassCastException: Double cannot be cast to Integer`.
sealed class Shape {
    abstract fun area(): Double
}

class Circle(val radius: Double) : Shape() {
    override fun area(): Double = 3.14 * radius * radius
}

class Square(val side: Double) : Shape() {
    override fun area(): Double = side * side
}

fun main() {
    val shapes: List<Shape> = listOf(Circle(1.0), Square(2.0))
    val total = shapes.fold(0.0) { acc, s -> acc + s.area() }
    println(total)
}
