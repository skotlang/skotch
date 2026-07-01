sealed class Result<T> {
    class Ok<T>(val v: T) : Result<T>()
    class Err<T>(val e: String) : Result<T>()
}

fun <T> Result<T>.describe(): String = when (this) {
    is Result.Ok -> "ok:$v"
    is Result.Err -> "err:$e"
}

fun main() {
    println(Result.Ok(7).describe())
    println(Result.Err<Int>("boom").describe())
}
