import kotlinx.coroutines.*

fun main() = runBlocking {
    var i = 3
    while (i > 0) {
        delay(10)
        println(i)
        i = i - 1
    }
}
