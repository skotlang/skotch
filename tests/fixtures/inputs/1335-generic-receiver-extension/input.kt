// Generic-receiver extension function: `fun <T> Iterable<T>.foo()`.
// Pre-fix the parser at parser.rs:~1812 read the receiver type
// identifier and immediately checked for `.` — it didn't allow
// `<...>` type args between the receiver ident and the dot. Result:
// `Iterable<T>.foo()` failed with "expected (, found Lt" at the
// `<` of the generic args.
//
// Fix at parser.rs:~1812 reads optional `<...>` after the receiver
// ident (saving position to roll back if the lookahead fails to
// reach a `.`), supporting type args including star projection
// `<*>` (desugared to `<Any>` at parse time).
//
// This fixture exercises the parser path only — the body uses
// receiver size (`this.size`) on `List<T>` rather than iteration
// (cross-extension-receiver iteration is a separate v0.50 gap
// that would obscure the parser fix).

fun <T> List<T>.sizeTimesTwo(): Int = this.size * 2

fun List<*>.starSizePlusOne(): Int = this.size + 1

fun main() {
    val nums = listOf(1, 2, 3, 4, 5)
    val strs = listOf("a", "b", "c")
    println(nums.sizeTimesTwo())                // 10
    println(strs.sizeTimesTwo())                // 6
    println(nums.starSizePlusOne())               // 6
    println(strs.starSizePlusOne())               // 4
}
