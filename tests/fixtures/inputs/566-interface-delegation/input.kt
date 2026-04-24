interface Logger {
    fun log(msg: String)
    fun level(): String
}

class ConsoleLogger : Logger {
    override fun log(msg: String) { println("[LOG] $msg") }
    override fun level(): String = "INFO"
}

class PrefixLogger(private val inner: Logger) : Logger by inner {
    override fun log(msg: String) {
        inner.log("[PREFIX] $msg")
    }
}

fun main() {
    val console = ConsoleLogger()
    val prefix = PrefixLogger(console)
    prefix.log("hello")
    println(prefix.level())
}
