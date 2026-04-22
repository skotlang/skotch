class Config {
    companion object {
        val VERSION: Int = 42
        val NAME: String = "skotch"
    }
}

fun main() {
    println(Config.VERSION)
    println(Config.NAME)
}
