interface Greeter {
    fun greet(): String
}

class HelloGreeter : Greeter {
    override fun greet(): String = "Hello!"
}

class ByeGreeter : Greeter {
    override fun greet(): String = "Goodbye!"
}

fun main() {
    val h = HelloGreeter()
    val b = ByeGreeter()
    println(h.greet())
    println(b.greet())
}
