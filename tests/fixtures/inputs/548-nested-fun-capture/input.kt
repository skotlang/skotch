fun main() {
    val x = 42
    fun double(): Int = x * 2
    println(double())

    val greeting = "Hello"
    fun greet(name: String): String = "$greeting, $name!"
    println(greet("World"))
}
