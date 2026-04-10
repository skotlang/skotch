interface Greeter {
    fun greeting(): String = "Hello"
    fun greet(name: String): String = "${greeting()}, $name!"
}

class FormalGreeter : Greeter {
    override fun greeting(): String = "Good day"
}

fun main() {
    val g: Greeter = FormalGreeter()
    println(g.greet("Alice"))
}
