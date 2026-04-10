// TODO: open class with override.
open class Animal {
    open fun speak() {
        println("...")
    }
}

class Dog : Animal() {
    override fun speak() {
        println("woof")
    }
}
