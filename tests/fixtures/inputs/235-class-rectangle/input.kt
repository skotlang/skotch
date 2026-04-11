class Rectangle(val width: Int, val height: Int) {
    fun area(): Int = width * height
    fun perimeter(): Int = 2 * (width + height)
}

fun main() {
    val r = Rectangle(5, 3)
    println(r.area())
    println(r.perimeter())
    println(r.width)
}
