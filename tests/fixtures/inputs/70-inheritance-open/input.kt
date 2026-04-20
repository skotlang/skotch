open class Animal {
    open fun speak(): String = "..."
}

class Dog : Animal() {
    override fun speak(): String = "Woof!"
}

class Cat : Animal() {
    override fun speak(): String = "Meow!"
}

fun main() {
    val d = Dog()
    val c = Cat()
    println(d.speak())
    println(c.speak())
}
