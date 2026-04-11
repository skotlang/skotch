fun greet(name: String): String {
    return "Hello, $name!"
}

fun describe(n: Int): String = "Number: $n"

fun main() {
    println(greet("Kotlin"))
    println(describe(42))
}
