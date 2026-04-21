sealed class Shape
class Circle : Shape()
class Square : Shape()

fun name(s: Shape): String = when (s) {
    is Circle -> "circle"
    // Missing: is Square -> should be compile error
}

fun main() {
    println(name(Circle()))
}
