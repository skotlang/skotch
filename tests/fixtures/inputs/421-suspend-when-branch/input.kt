import kotlinx.coroutines.*

suspend fun fetch(code: Int): String {
    when (code) {
        1 -> { delay(10); return "one" }
        2 -> { delay(20); return "two" }
        else -> { return "other" }
    }
}

fun main() = runBlocking {
    println(fetch(1))
    println(fetch(2))
    println(fetch(3))
}
