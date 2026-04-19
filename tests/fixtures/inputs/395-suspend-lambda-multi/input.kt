suspend fun yield_(): Unit {}

fun makeLambda(): Any {
    val block = {
        val x = 10
        yield_()
        val y = 20
        yield_()
        x + y
    }
    return block
}
