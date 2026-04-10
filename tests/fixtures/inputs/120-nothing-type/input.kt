fun fail(message: String): Nothing {
    throw IllegalStateException(message)
}

fun main() {
    try {
        fail("oops")
    } catch (e: IllegalStateException) {
        println("caught: ${e.message}")
    }
}
