sealed class Shape
class Circle : Shape()
class Square : Shape()

fun name(s: Shape): String = when (s) {
    is Circle -> "circle"
    is Square -> "square"
}

fun main() {
    println(name(Circle()))
    println(name(Square()))
}
