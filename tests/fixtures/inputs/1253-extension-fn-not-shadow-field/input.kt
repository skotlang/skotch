// Regression: an extension function whose simple name matches an
// instance field on a DIFFERENT class must not shadow the field.
//
// Before the fix, `fun Int.cents(): Money` registered as a top-level
// function named `cents`; the Field lowering picked that up via
// `name_to_func.get("cents")` and emitted a static call
// `cents(otherMoney)` (typed Long → Long-mismatched, then stubbed),
// dropping the user-written `add` body. The fix:
//
//   (a) skotch-mir-lower at function-pre-allocation: the placeholder
//       locals for the extension's `this`-receiver param were always
//       `Ty::Any` (offset bug — typeck's `param_tys` already includes
//       the receiver as index 0). Receiver type is now preserved.
//
//   (b) Field lowering's `params_len == 1` branch now checks the
//       actual receiver type against the extension's declared
//       receiver type before treating the call as an extension; on
//       mismatch it falls through to field/getter resolution.
data class Money(val cents: Long) {
    fun add(other: Money): Money = Money(cents + other.cents)
}

fun Int.cents(): Money = Money(this.toLong())

fun main() {
    val a = Money(10L)
    val b = Money(20L)
    println(a.add(b).cents)
    println(30.cents().cents)
}
