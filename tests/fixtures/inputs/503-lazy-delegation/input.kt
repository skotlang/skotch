class Config {
    val value: String by lazy {
        println("initializing")
        "computed"
    }
}

fun main() {
    val c = Config()
    println("before access")
    println(c.value)
    println(c.value)
}
