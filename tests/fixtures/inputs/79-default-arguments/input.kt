fun greet(name: String = "world", greeting: String = "Hello") {
    println("$greeting, $name!")
}

fun main() {
    greet()
    greet("Kotlin")
    greet("Kotlin", "Hi")
}
