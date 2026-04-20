sealed class Shape
class Circle(val name: String) : Shape()
class Square(val side: Int) : Shape()

fun describe(s: Shape): String = when (s) {
    is Circle -> "circle: ${s.name}"
    is Square -> "square"
}

fun main() {
    println(describe(Circle("big")))
    println(describe(Square(5)))
}
