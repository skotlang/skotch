// Local function (`fun helper`) that captures BOTH `s` (val) and
// `builder` (val pointing at mutable state) from the enclosing
// scope AND recurses into itself. Pre-fix, two bugs:
//
//   1. Outer-call site looked up captures with
//      `scope.find($capture$fn$*)` — `find()` returned the FIRST
//      match for every slot, so all captures collapsed to the
//      most-recently-pushed one (e.g. `s, s` instead of
//      `builder, s`).
//
//   2. Recursive self-call inside the local fn body found NO
//      `$capture$` keys in inner_scope, so the capture-forwarding
//      loop didn't execute at all → the recursive call passed only
//      the user arg (`i - 1`) instead of forwarding `builder, s,
//      i-1`. The JVM emitter then boxed the lone Int and emitted a
//      checkcast to StringBuilder → VerifyError.
//
// Fix: populate MirFunction.param_names with capture names (on the
// placeholder too, so recursive calls during inner-fn lowering see
// them), then at the call site look up each capture by its source-
// level name. Works for both outer scope (the original locals are
// in scope under that name) and inner_scope (the local fn's own
// params are bound under that name).

fun reverse(s: String): String {
    val builder = StringBuilder()
    fun helper(i: Int) {
        if (i < 0) return
        builder.append(s[i])
        helper(i - 1)
    }
    helper(s.length - 1)
    return builder.toString()
}

fun main() {
    println(reverse("hello"))
    println(reverse("kotlin"))
}
