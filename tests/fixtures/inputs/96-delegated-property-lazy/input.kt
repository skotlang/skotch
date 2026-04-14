class Config {
    val greeting: String by lazy {
        println("computing...")
        "Hello!"
    }
}

fun main() {
    val c = Config()
    println(c.greeting)
    println(c.greeting)
}
