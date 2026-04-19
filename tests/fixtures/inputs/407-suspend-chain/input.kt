import kotlinx.coroutines.*

suspend fun fetchData(): String {
    delay(10)
    return "data"
}

suspend fun processData(): String {
    val raw = fetchData()
    return "processed: " + raw
}

fun main() = runBlocking {
    println(processData())
}
