// Locks in element-type flow across chained collection calls.
//
// `users.map { it.name }` is `List<String>` (lambda returns String).
// `.filter { it.length > 3 }` then sees `it: String` and `.length`
// resolves against String. Without the result-element-type
// propagation in skotch-mir-lower, the filter lambda's `it` falls
// back to `Any`, `.length` fails, and the predicate is silently
// dropped — fixture 1216 catches the single-call case; this one
// catches the chained case.

data class User(val name: String, val age: Int)

fun main() {
    val users = listOf(
        User("Ada", 36),
        User("Bo", 24),
        User("Cyril", 51),
    )
    val longNames = users.map { it.name }.filter { it.length > 3 }
    for (n in longNames) {
        println(n)
    }
}
