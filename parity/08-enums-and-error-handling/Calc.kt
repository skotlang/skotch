// Custom exception class — extends RuntimeException via the
// `: RuntimeException(message)` superclass-constructor form.
class CalcError(message: String) : RuntimeException(message)

// Object singleton with one method. `Calculator.evaluate(...)` is
// compiled as `getstatic Calculator.INSTANCE; invokevirtual
// Calculator.evaluate`. Multiple uses share the singleton.
object Calculator {
    fun evaluate(a: Int, op: Op, b: Int): Int {
        if (op == Op.DIV && b == 0) {
            throw CalcError("divide by zero")
        }
        return applyOp(op, a, b)
    }
}
