open class Animal {
    open fun speak(): String = "..."
}

class Dog : Animal() {
    override fun speak(): String = "Woof"
}

class Cat : Animal() {
    override fun speak(): String = "Meow"
}

fun main() {
    val dog = Dog()
    val cat = Cat()
    println(dog.speak())
    println(cat.speak())
}
