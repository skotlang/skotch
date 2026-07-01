sealed interface Shape {
    fun area(): Double
}

class Square(val side: Double) : Shape {
    override fun area(): Double = side * side
}

class Circle(val r: Double) : Shape {
    override fun area(): Double = 3.14 * r * r
}

fun describe(s: Shape): String = when (s) {
    is Square -> "sq:${s.area()}"
    is Circle -> "ci:${s.area()}"
}

fun main() {
    println(describe(Square(3.0)))
    println(describe(Circle(2.0)))
}
