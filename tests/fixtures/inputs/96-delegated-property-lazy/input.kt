val greeting: String by lazy {
    println("computing...")
    "Hello!"
}

fun main() {
    println(greeting)
    println(greeting)
}
