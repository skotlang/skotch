import kotlinx.coroutines.*

fun main() = runBlocking {
    launch {
        delay(50)
        println("world")
    }
    println("hello")
    delay(200)
}
