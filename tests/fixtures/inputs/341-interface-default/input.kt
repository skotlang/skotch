interface Greeter {
    fun greeting(): String = "Hello"
    fun greet(name: String): String = greeting() + ", " + name
}

class Formal : Greeter {
    override fun greeting(): String = "Good day"
}

fun main() {
    println(Formal().greet("Alice"))
}
