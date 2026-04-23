class Dog(val name: String) {
    fun bark(): String = "$name says Woof!"
}
class Cat(val name: String) {
    fun meow(): String = "$name says Meow!"
}
fun main() {
    println(Dog("Rex").bark())
    println(Cat("Whiskers").meow())
}
