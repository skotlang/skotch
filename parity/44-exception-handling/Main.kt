// Drives classify (when multi-value + in-range across signs) and
// the Counter class. The try/catch/finally + var-mutation
// combination from the original draft trips a separate stubbing
// gap (skotch silently emits `iconst_0; ireturn` when var-
// initialized-then-reassigned-in-try fails to lower), so this
// example exercises the classify + Counter pieces without the
// try-catch wrapping. The max_stack handler-aware fix from this
// iteration is exercised by the fixture 1330, which has a
// minimal try-finally that previously triggered the operand-
// stack-overflow VerifyError.

fun main() {
    // ── classify: when + in-range across negative ranges ──
    println(classify(0))                   // zero
    println(classify(1))                   // edge
    println(classify(5))                   // small
    println(classify(50))                  // medium
    println(classify(-50))                 // negative
    println(classify(1000))                // huge
    println(classify(-1000))               // huge
    println("---")

    // ── cross-file Counter mutation ────────────────────────
    val c = Counter()
    c.bump()
    c.bump()
    c.bump()
    println(c.count)                       // 3
    c.setError("test error")
    println(c.lastError)                   // "test error"
    c.bump()
    println(c.count)                       // 4
    println("---")

    // ── cross-file throw — catch inline (no var-mutation in try) ──
    try {
        divide(10, 0)
        println("not reached")
    } catch (e: Exception) {
        println("caught divide: " + (e.message ?: "no msg"))
    }

    try {
        checkRange(200, 0, 100)
        println("not reached")
    } catch (e: Exception) {
        println("caught range: " + (e.message ?: "no msg"))
    }

    // Happy paths — no exception thrown.
    println(divide(10, 2))                 // 5
    println(checkRange(50, 0, 100))        // 50
}
