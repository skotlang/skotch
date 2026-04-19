import kotlinx.coroutines.*

fun main() = runBlocking {
    val result = withContext(Dispatchers.Default) {
        delay(10)
        42
    }
    println(result)
}
