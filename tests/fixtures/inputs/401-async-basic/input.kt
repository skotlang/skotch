import kotlinx.coroutines.*

fun main() = runBlocking {
    async {
        println("in async")
    }
    delay(100)
    println("done")
}
