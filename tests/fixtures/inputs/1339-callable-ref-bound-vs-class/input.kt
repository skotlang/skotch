// Bound-instance callable ref to a zero-arg method (`counter::inc` for
// a `() -> Unit` slot). Pre-fix the parser at parser.rs:~3887
// unconditionally desugared every `::` to a 1-arg lambda
// `{ $ref_arg -> lhs.member($ref_arg) }`, and the call-site arity
// adapter at mir-lower:~3037 short-circuited on `expected_arity == 1`
// so it never shrank the lambda when the target slot expected 0
// params. Runtime: `ClassCastException Function1 cannot be cast to
// Function0`.
//
// Fix at mir-lower:~3037 (`maybe_expand_callable_ref`) keeps the
// existing `expected_arity == 1` early-out (already correct) and
// allows the 0-arity case to fall through to the expansion loop —
// which now legitimately builds zero params / zero args, dropping
// the spurious `$ref_arg`. The arity > 1 path is unchanged.
//
// CROSS-FILE limitation remains: when the calling function lives in
// a different file from `applyTimes`, name_to_func.get() returns
// None so param_tys stays empty and maybe_expand_callable_ref isn't
// invoked. That's a separate gap; see milestones.yaml v0.50 entry.

class Counter(private var n: Int) {
    fun inc() {
        n += 1
    }
    fun value(): Int = n
}

fun applyTimes(times: Int, action: () -> Unit) {
    var i = 0
    while (i < times) {
        action()
        i += 1
    }
}

fun main() {
    val c = Counter(0)
    applyTimes(5, c::inc)
    println("counter=${c.value()}")
}
