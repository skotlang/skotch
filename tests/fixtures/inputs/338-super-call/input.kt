open class Base {
    open fun greet(): String = "Hello"
}

class Child : Base() {
    override fun greet(): String = super.greet() + " World"
}

fun main() {
    println(Child().greet())
}
