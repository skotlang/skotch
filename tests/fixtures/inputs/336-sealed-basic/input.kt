sealed class Result

class Ok(val value: Int) : Result()
class Err(val message: String) : Result()

fun describe(r: Result): String = when {
    r is Ok -> "ok"
    r is Err -> "err"
    else -> "unknown"
}

fun main() {
    println(describe(Ok(42)))
    println(describe(Err("fail")))
}
