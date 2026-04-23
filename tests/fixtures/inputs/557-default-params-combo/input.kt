fun greet(name: String = "World", prefix: String = "Hello"): String {
    return "$prefix, $name!"
}

fun main() {
    println(greet())
    println(greet("Kotlin"))
    println(greet("Kotlin", "Hi"))
}
