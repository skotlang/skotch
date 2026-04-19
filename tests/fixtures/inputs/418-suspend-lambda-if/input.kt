import kotlinx.coroutines.*

fun main() = runBlocking {
    val flag = true
    if (flag) {
        delay(10)
        println("yes")
    } else {
        delay(20)
        println("no")
    }
}
