import kotlinx.coroutines.*

suspend fun fetchValue(): Int {
    delay(10)
    return 42
}

fun main() = runBlocking {
    println(fetchValue())
}
