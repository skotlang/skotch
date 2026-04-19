import kotlinx.coroutines.*

fun main() = runBlocking {
    val msg = "hello from launch"
    launch {
        delay(10)
        println(msg)
    }
    delay(200)
}
