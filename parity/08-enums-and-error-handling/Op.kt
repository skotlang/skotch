// Simple enum class with constructor params (the `symbol` field).
// Enum entries become `public static final Op` singletons; the
// constructor stores `name` and `symbol` in instance fields.
enum class Op(val symbol: String) {
    PLUS("+"),
    MINUS("-"),
    TIMES("*"),
    DIV("/"),
}

// Top-level function dispatching on the enum entry. Uses a `when`
// expression with exhaustive case coverage — no `else` needed because
// the typechecker proves every `Op` value is matched.
fun applyOp(op: Op, a: Int, b: Int): Int = when (op) {
    Op.PLUS -> a + b
    Op.MINUS -> a - b
    Op.TIMES -> a * b
    Op.DIV -> a / b
}
