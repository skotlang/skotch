sealed class Result
class Success(val value: String) : Result()
class Failure(val error: String) : Result()

fun describe(r: Result): String = when (r) {
    is Success -> "OK: ${r.value}"
    is Failure -> "ERR: ${r.error}"
}

fun main() {
    println(describe(Success("hello")))
    println(describe(Failure("oops")))
}
