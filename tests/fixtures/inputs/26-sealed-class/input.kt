sealed class Result
class Ok(val value: Int) : Result()
class Err(val message: String) : Result()

fun main() {
    val r: Result = Ok(42)
    println(r is Ok)
}
