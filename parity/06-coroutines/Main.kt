import kotlinx.coroutines.*

suspend fun fetchUser(id: Int): String {
    delay(20)
    return "user#$id"
}

suspend fun fetchScore(id: Int): Int {
    delay(20)
    return id * 100
}

fun main() = runBlocking {
    println("start")

    val user = fetchUser(1)
    println("got $user")

    // Launch two concurrent jobs and await their joins.
    val job1 = launch {
        delay(15)
        println("job1 done")
    }
    val job2 = launch {
        delay(10)
        println("job2 done")
    }
    job1.join()
    job2.join()

    // Run two fetches in parallel with async/await.
    val u = async { fetchUser(42) }
    val s = async { fetchScore(42) }
    println("parallel result: ${u.await()} score=${s.await()}")

    println("done")
}
