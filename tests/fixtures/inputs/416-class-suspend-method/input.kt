import kotlinx.coroutines.*

class Fetcher {
    suspend fun fetch(): String {
        delay(10)
        return "data"
    }
}

fun main() = runBlocking {
    val f = Fetcher()
    val result = f.fetch()
    println(result)
}
