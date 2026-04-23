abstract class Animal(val name: String) {
    abstract fun sound(): String
}

class Dog(name: String) : Animal(name) {
    override fun sound(): String = "Woof"
}

class Cat(name: String) : Animal(name) {
    override fun sound(): String = "Meow"
}

fun main() {
    val d = Dog("Rex")
    println(d.name)
    println(d.sound())
    val c = Cat("Whiskers")
    println(c.name)
    println(c.sound())
}
