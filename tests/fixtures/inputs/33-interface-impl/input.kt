interface Greeter {
    fun greet(): String
}

class Hello : Greeter {
    override fun greet(): String = "Hello!"
}

fun main() {
    val g = Hello()
    println(g.greet())
}
