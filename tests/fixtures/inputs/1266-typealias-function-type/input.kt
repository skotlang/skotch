// Regression: a `typealias` whose target is a function type must
// resolve to the function type (NOT just the return-type's name)
// everywhere — typeck, MIR field-type computation, AND resolve
// crate's cross-file descriptor builder. Before the fix:
//   - typeck stored alias as name-only → expected `Boolean` for
//     `Predicate` slot and failed assignment from a lambda.
//   - mir-lower computed field type as Bool → emitted `pred: Z`
//     and constructor `(Z)V`.
//   - resolve's `type_ref_to_descriptor` defaulted to `Object`
//     because `Predicate` wasn't a known stdlib name → cross-file
//     callers built `<init>(Object)` and `NoSuchMethodError`d.
typealias Predicate = (Int) -> Boolean

class Holder(private val pred: Predicate) {
    fun call(n: Int): Boolean = pred(n)
}

fun main() {
    val isEven: Predicate = { n -> n % 2 == 0 }
    val h = Holder(isEven)
    println(h.call(2))
    println(h.call(3))
}
