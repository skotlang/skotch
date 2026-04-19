suspend fun yield_(): Unit {}

suspend fun run(): Int {
    val x = 10
    yield_()
    val y = 20
    yield_()
    return x + y
}
