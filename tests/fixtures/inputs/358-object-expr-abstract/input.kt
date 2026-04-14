abstract class Handler {
    abstract fun handle(msg: String): String
}

fun main() {
    val h = object : Handler() {
        override fun handle(msg: String): String = "Got: " + msg
    }
    println(h.handle("test"))
}
