import kotlinx.coroutines.*

fun main() = runBlocking {
    val job = launch {
        delay(50)
        println("child done")
    }
    println("waiting")
    job.join()
    println("all done")
}
