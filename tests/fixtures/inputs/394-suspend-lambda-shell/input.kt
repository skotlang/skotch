suspend fun yield_(): Unit {}

fun makeLambda(): Any {
    val block = {
        yield_()
        "hello"
    }
    return block
}
