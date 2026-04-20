open class Animal {
    open fun speak(): String = "..."
}

class Dog : Animal() {
    override fun speak(): String = "woof"
}

fun main() {
    val d = Dog()
    println(d.speak())
}
