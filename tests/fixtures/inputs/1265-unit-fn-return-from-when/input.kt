// Regression: a `Unit`-returning function whose body is (or ends in)
// a `when` expression whose branches each "produce" a value must
// emit `return` (0xB1), not `areturn` (0xB0). Before the fix, the
// MIR's `Terminator::ReturnValue` was always lowered as `areturn`,
// and Unit locals push nothing onto the stack — verifier rejected
// the method as "Operand stack underflow".
fun describe(n: Int) {
    when (n) {
        0 -> println("zero")
        1 -> println("one")
        else -> println("many")
    }
}

fun main() {
    describe(0)
    describe(1)
    describe(7)
}
