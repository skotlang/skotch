sealed class Shape
class Circle(val radius: Double) : Shape()
class Rectangle(val w: Double, val h: Double) : Shape()

fun area(s: Shape): Double = when (s) {
    is Circle -> 3.14159 * s.radius * s.radius
    is Rectangle -> s.w * s.h
}

fun main() {
    println(area(Circle(1.0)))
    println(area(Rectangle(3.0, 4.0)))
}
