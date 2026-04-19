import kotlinx.coroutines.*

fun main() = runBlocking {
    println("before yield")
    yield()
    println("after yield")
}
