sealed class Shape
class Circle : Shape()
class Rect : Shape()

fun name(s: Shape): String = when (s) {
    is Circle -> "circle"
    is Rect -> "rect"
    else -> "unknown"
}

fun main() {
    println(name(Circle()))
    println(name(Rect()))
}
