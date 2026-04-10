open class Animal(val name: String) {
    open fun speak(): String = "..."
}

class Dog(name: String) : Animal(name) {
    override fun speak(): String = "Woof!"
}

class Cat(name: String) : Animal(name) {
    override fun speak(): String = "Meow!"
}

fun main() {
    val animals: List<Animal> = listOf(Dog("Rex"), Cat("Whiskers"))
    for (a in animals) {
        println("${a.name} says ${a.speak()}")
    }
}
