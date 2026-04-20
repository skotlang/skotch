object Config {
    fun greeting(): String = "Hello from singleton!"
    fun version(): Int = 42
}

fun main() {
    println(Config.greeting())
    println(Config.version())
}
