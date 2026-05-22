// Locks in `.let { ... }` implicit `it` parameter type inference
// when the receiver is a class with members. Without inference,
// `it.name` in the lambda body fails to resolve and the lambda
// body is silently dropped.

data class User(val name: String, val age: Int)

fun describe(u: User): String = "${u.name}/${u.age}"

fun main() {
    val u = User("Ada", 36)
    val out = u.let { describe(it) }
    println(out)
}
