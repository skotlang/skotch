sealed class Shape {
    abstract fun area(): Double
}

class Circle(val radius: Double) : Shape() {
    override fun area(): Double = 3.14159 * radius * radius
}

class Rectangle(val width: Double, val height: Double) : Shape() {
    override fun area(): Double = width * height
}

class Triangle(val base: Double, val height: Double) : Shape() {
    override fun area(): Double = 0.5 * base * height
}

fun describe(s: Shape): String = when (s) {
    is Circle    -> "circle r=${s.radius}"
    is Rectangle -> "rect ${s.width}x${s.height}"
    is Triangle  -> "tri base=${s.base} h=${s.height}"
}
