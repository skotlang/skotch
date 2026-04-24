fun greet(name: String): String = "Hello, $name!"

fun main() {
    val items = listOf(1, 2, 3, 4, 5)
    println("Count: ${items.size}")
    println("Greeting: ${greet("World")}")
    val x = 10
    val y = 20
    println("Sum: ${x + y}, Product: ${x * y}")
    println("Items: ${items.joinToString(", ")}")
    println("Reversed: ${items.reversed()}")
    println("First three: ${items.take(3)}")
}
