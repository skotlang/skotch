open class Base(val x: Int)

class Child(val y: Int) : Base(y * 2)

fun main() {
    val c = Child(5)
    println(c.y)
}
