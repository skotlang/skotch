abstract class Shape {
    abstract fun area(): Int
}

class Square(val side: Int) : Shape() {
    override fun area(): Int = side * side
}

class Rect(val w: Int, val h: Int) : Shape() {
    override fun area(): Int = w * h
}

fun main() {
    val s = Square(5)
    val r = Rect(3, 4)
    println(s.area())
    println(r.area())
}
