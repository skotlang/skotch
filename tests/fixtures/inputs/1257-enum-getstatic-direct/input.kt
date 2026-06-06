// Regression: enum entry access `Op.PLUS` must emit
// `getstatic Op.PLUS:LOp;` directly when there's no same-file
// accessor function registered (the cross-file case, OR a same-file
// case where the accessor lookup fails). Before the fix,
// `Op.PLUS` from a different file lowered to an instance field
// access on a null receiver with descriptor `Object` — runtime
// `NoSuchFieldError: Op does not have member field
// 'java.lang.Object PLUS'`.
enum class Op {
    PLUS,
    MINUS,
    TIMES,
    DIV,
}

fun describe(op: Op): String = when (op) {
    Op.PLUS -> "plus"
    Op.MINUS -> "minus"
    Op.TIMES -> "times"
    Op.DIV -> "div"
}

fun main() {
    println(describe(Op.PLUS))
    println(describe(Op.DIV))
}
