class Greeter(val name: String) {
    init {
        println("Creating greeter for $name")
    }

    fun greet(): String = "Hello, $name!"
}

fun main() {
    val g = Greeter("World")
    println(g.greet())
}
