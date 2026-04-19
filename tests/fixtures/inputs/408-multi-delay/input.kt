import kotlinx.coroutines.*

fun main() = runBlocking {
    println("start")
    delay(10)
    println("middle")
    delay(10)
    println("end")
}
