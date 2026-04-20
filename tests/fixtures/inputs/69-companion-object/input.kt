class Config {
    companion object {
        fun defaultName(): String = "skotch"
    }
}

fun main() {
    println(Config.defaultName())
}
