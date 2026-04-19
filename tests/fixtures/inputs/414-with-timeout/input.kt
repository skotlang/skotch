import kotlinx.coroutines.*

fun main() = runBlocking {
    val result = withTimeout(5000) {
        delay(10)
        "completed"
    }
    println(result)
}
