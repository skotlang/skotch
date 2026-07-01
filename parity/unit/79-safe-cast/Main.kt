open class Animal
class Dog(val name: String) : Animal()
class Cat(val name: String) : Animal()

fun describe(a: Animal): String {
    val d = a as? Dog
    if (d != null) return "dog:${d.name}"
    val c = a as? Cat
    return c?.let { "cat:${it.name}" } ?: "unknown"
}

fun main() {
    println(describe(Dog("rex")))
    println(describe(Cat("tom")))
    println(describe(Animal()))
}
