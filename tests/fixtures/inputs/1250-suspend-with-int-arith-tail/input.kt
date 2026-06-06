// Regression: a suspend function whose post-resume tail is an
// arithmetic expression using a captured user parameter. The earlier
// fix added `MakeConcatWithConstants` to the suspend segment emitter;
// this fixture also exercises the broader pattern where the tail
// computes a Boxing call (return id * 100 → boxInt(id*100)) on top of
// the spilled parameter restore.
import kotlinx.coroutines.*

suspend fun fetchScore(id: Int): Int {
    delay(20)
    return id * 100
}

fun main() = runBlocking {
    println(fetchScore(42))
}
