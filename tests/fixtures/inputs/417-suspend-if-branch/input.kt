import kotlinx.coroutines.*

suspend fun pick(flag: Boolean): Int {
    if (flag) {
        delay(10)
        return 1
    } else {
        delay(20)
        return 2
    }
}

fun main() = runBlocking {
    println(pick(true))
    println(pick(false))
}
