suspend fun yield_(): Unit {}

suspend fun runIt(block: suspend () -> String): String {
    return block()
}

suspend fun run_(): String {
    return runIt {
        yield_()
        "hello"
    }
}

fun main() {
    println("suspend runtime wired")
}
