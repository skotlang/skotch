// Regression: `for (s in shapes)` where `shapes: List<Shape>` must
// type the loop variable as `Shape` and emit a checkcast so
// `s.area()` dispatches virtually to the concrete subclass's
// implementation. Before the fix, the loop variable was typed `Any`,
// `s.area()` lowered to `Const(null)`, and the string template
// printed `area=null`.
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
    for (s in shapes) {
        println(s.area())
    }
}
