import kotlinx.coroutines.*

fun main() = runBlocking {
    val d = async { 42 }
    val r = d.await()
    println(r)
}
