class Circle(val radius: Double) {
    val area: Double
        get() = 3.14159 * radius * radius
    val diameter: Double
        get() = radius * 2.0
}

fun main() {
    val c = Circle(5.0)
    println(c.area)
    println(c.diameter)
}
