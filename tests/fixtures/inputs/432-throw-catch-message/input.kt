fun fail(msg: String): Nothing {
    throw IllegalStateException(msg)
}

fun main() {
    try {
        fail("test error")
    } catch (e: IllegalStateException) {
        println("caught: ${e.message}")
    }
}
