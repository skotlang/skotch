abstract class Shape {
    abstract fun area(): Double
    fun describe(): String = "Shape with area ${area()}"
}

class Square(val side: Double) : Shape() {
    override fun area(): Double = side * side
}

fun main() {
    val s = Square(5.0)
    println(s.describe())
}
