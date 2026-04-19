import kotlinx.coroutines.*

fun main() {
    val message = "hello from coroutine"
    runBlocking {
        delay(10)
        println(message)
    }
}
