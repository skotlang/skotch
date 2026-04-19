import kotlinx.coroutines.*

fun main() = runBlocking {
    val a = async { delay(10); 1 }
    val b = async { delay(10); 2 }
    println(a.await() + b.await())
}
