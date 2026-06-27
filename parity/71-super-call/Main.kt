open class Base {
    open fun greet(): String = "hello"
}

class Sub : Base() {
    override fun greet(): String = super.greet() + ", world"
}

fun main() {
    println(Base().greet())
    println(Sub().greet())
}
