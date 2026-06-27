interface Greeter {
    fun greet(): String
}

fun makeGreeter(name: String): Greeter = object : Greeter {
    override fun greet(): String = "hello, $name"
}

fun main() {
    val g1 = makeGreeter("world")
    val g2 = makeGreeter("kotlin")
    println(g1.greet())
    println(g2.greet())
}
