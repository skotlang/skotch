// Generic-receiver extension function: `fun <T> Iterable<T>.foo()`.
// Pre-fix the parser at parser.rs:~1812 read the receiver type
// identifier and immediately checked for `.` — it didn't allow
// `<...>` type args between the receiver ident and the dot. Result:
// `Iterable<T>.countWhere` failed with "expected (, found Lt" at
// the `<` of the generic args. Star projection `Iterable<*>` had
// the same failure mode.
//
// Fix at parser.rs:~1812 reads optional `<...>` after the receiver
// ident (saving position to roll back if the lookahead fails to
// reach a `.`), supporting both ordinary type args (`<T>`) and
// star projections (`<*>`, which desugars to `<Any>` at the MIR
// level since skotch erases generics).
//
// Surfaced by parity/49-functional-pipelines.

inline fun <T> Iterable<T>.countWhere(predicate: (T) -> Boolean): Int {
    var n = 0
    for (item in this) {
        if (predicate(item)) {
            n = n + 1
        }
    }
    return n
}

fun <T> Iterable<T>.firstOrNullByPredicate(predicate: (T) -> Boolean): T? {
    for (item in this) {
        if (predicate(item)) return item
    }
    return null
}

fun main() {
    val nums = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    println(nums.countWhere { it % 2 == 0 })
    println(nums.firstOrNullByPredicate { it > 5 })
}
