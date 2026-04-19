import kotlinx.coroutines.*

suspend fun doubleAfterDelay(x: Int): Int {
    delay(10)
    return x * 2
}

fun main() = runBlocking {
    val a = doubleAfterDelay(5)
    val b = doubleAfterDelay(10)
    println("sequential: " + (a + b).toString())
    val da = async { doubleAfterDelay(100) }
    val db = async { doubleAfterDelay(200) }
    println("parallel: " + (da.await() + db.await()).toString())
    val msg = "launched"
    launch {
        delay(10)
        println(msg)
    }
    delay(100)
}
