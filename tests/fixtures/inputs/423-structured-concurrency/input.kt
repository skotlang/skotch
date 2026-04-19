import kotlinx.coroutines.*

fun main() = runBlocking {
    coroutineScope {
        launch {
            delay(30)
            println("child")
        }
        println("parent")
    }
    println("done")
}
