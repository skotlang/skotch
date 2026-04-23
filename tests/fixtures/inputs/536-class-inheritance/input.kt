open class Shape(val name: String) {
    open fun area(): Int = 0
    override fun toString(): String = "$name(area=${area()})"
}

class Square(val side: Int) : Shape("Square") {
    override fun area(): Int = side * side
}

fun main() {
    val s = Square(5)
    println(s.area())
    println(s.toString())
    println(s.name)
}
