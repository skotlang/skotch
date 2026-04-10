import kotlinx.coroutines.*

fun main() = runBlocking {
    val job = launch {
        delay(10)
        println("launched")
    }
    job.join()
    println("done")
}
