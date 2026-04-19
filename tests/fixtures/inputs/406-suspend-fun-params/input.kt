import kotlinx.coroutines.*

suspend fun compute(x: Int): Int {
    delay(10)
    return x * 2
}

fun main() = runBlocking {
    val a = compute(5)
    val b = compute(10)
    println(a + b)
}
