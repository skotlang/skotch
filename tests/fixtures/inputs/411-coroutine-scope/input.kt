import kotlinx.coroutines.*

suspend fun sumWithScope(): Int = coroutineScope {
    val a = async { delay(10); 10 }
    val b = async { delay(10); 20 }
    a.await() + b.await()
}

fun main() = runBlocking {
    val result = sumWithScope()
    println(result)
}
