class Config {
    lateinit var name: String
    fun init(n: String) { name = n }
    fun show(): String = "config:$name"
}

fun main() {
    val c = Config()
    c.init("alpha")
    println(c.show())
    c.init("beta")
    println(c.show())
}
