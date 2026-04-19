import kotlinx.coroutines.*

suspend fun safeFetch(): String {
    try {
        delay(10)
        return "ok"
    } catch (e: Exception) {
        return "fail"
    }
}

fun main() = runBlocking {
    println(safeFetch())
}
