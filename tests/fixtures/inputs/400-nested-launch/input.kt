import kotlinx.coroutines.*

fun main() = runBlocking {
    launch {
        println("launched")
    }
    delay(100)
    println("done")
}
