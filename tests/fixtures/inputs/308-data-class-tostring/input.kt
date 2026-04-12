data class Config(val host: String, val port: Int)

fun main() {
    val c = Config("localhost", 8080)
    println(c)
    val s = c.toString()
    println(s)
}
