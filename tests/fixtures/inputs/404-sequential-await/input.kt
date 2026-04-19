import kotlinx.coroutines.*

fun main() = runBlocking {
    val a = async { 10 }
    val x = a.await()
    println(x)
    val b = async { 20 }
    val y = b.await()
    println(y)
}
