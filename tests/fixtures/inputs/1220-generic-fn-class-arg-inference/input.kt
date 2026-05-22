// Locks in: `fun <T> identity(x: T): T = x` called with a class
// argument infers T = that class, and downstream member access on
// the result type-checks.
//
// Without inference, `identity(Wrapper("hi"))` would return `Any`,
// `.s` wouldn't resolve, and the whole `println` statement would be
// silently dropped.

data class Wrapper(val s: String)

fun <T> identity(x: T): T = x

fun main() {
    val w = identity(Wrapper("hi"))
    println(w.s)
}
