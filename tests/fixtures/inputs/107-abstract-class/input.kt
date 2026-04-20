abstract class Shape {
    abstract fun name(): String
}

class Circle : Shape() {
    override fun name(): String = "circle"
}

fun main() {
    val s: Shape = Circle()
    println(s.name())
}
