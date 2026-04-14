interface Greeter {
    fun greet(): String
}

fun main() {
    val g = object : Greeter {
        override fun greet(): String = "Hello from anonymous!"
    }
    println(g.greet())
}
