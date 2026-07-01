interface Animal {
    fun name(): String
    fun greet(): String = "Hi, I'm ${name()}"
}

class Dog2 : Animal {
    override fun name(): String = "Rex"
}

class Cat2 : Animal {
    override fun name(): String = "Tom"
    override fun greet(): String = "Meow, ${name()}"
}

fun main() {
    println(Dog2().greet())
    println(Cat2().greet())
}
