class Config {
    companion object {
        fun appName(): String = "MyApp"
        fun version(): Int = 1
    }
}

fun main() {
    println(Config.appName())
    println(Config.version())
}
