// Regression: `fun run(): Unit { ... }` with an explicit `: Unit`
// annotation was getting its return type rewritten to `Int` by
// mir-lower's body-based inference. The inference looked at the
// last block's terminator (`ReturnValue(local)`) and used the
// local's type, ignoring the source-level annotation. Result:
// caller-side `v.run()` emitted a `pop` after a `void` call,
// crashing with "Operand stack underflow" at runtime.
//
// Fix: track `has_explicit_return_ty` in the lower_class loop;
// skip the body-inference fallback when the source annotated the
// type. Without `: Unit` (no annotation) the inference still
// fires for cases where the function returns a value implicitly.

class Counter {
    var pc: Int = 0
    fun bump(): Unit {
        pc++
    }
}

fun main() {
    val c = Counter()
    c.bump()
    c.bump()
    c.bump()
    println(c.pc)
}
