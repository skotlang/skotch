sealed class Shape
class Circle(val r: Int) : Shape()
class Rect(val w: Int, val h: Int) : Shape()

fun area(s: Shape): Int = when (s) {
    is Circle -> s.r * s.r * 3
    is Rect -> s.w * s.h
}

fun main() {
    println(area(Circle(5)))
    println(area(Rect(3, 4)))
}
