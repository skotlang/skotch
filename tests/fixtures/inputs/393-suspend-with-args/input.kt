suspend fun greet(name: String): String = name

suspend fun run(): String {
    val result = greet("World")
    return result
}
