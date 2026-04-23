interface Shape {
    fun area(): Double
}
class Circle(val radius: Double) : Shape {
    override fun area(): Double = 3.14159 * radius * radius
}
class Rectangle(val width: Double, val height: Double) : Shape {
    override fun area(): Double = width * height
}
fun printArea(shape: Shape) {
    println(shape.area())
}
fun main() {
    printArea(Circle(5.0))
    printArea(Rectangle(3.0, 4.0))
}
