import kotlinx.coroutines.*

fun main() = runBlocking {
    try {
        delay(10)
        println("done")
    } catch (e: Exception) {
        println("error")
    }
}
