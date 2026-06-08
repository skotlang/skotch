// Multi-arg `operator fun get(r, c)` and `operator fun set(r, c, v)`
// for Kotlin's natural `m[r, c]` / `m[r, c] = v` syntax. Pre-fix
// skotch's parser only accepted single-arg `[index]` — the second
// comma in `[r, c]` failed with `expected expression, found RBracket`.
//
// Fix at parser.rs:~3741 (read side): collect comma-separated
// indices inside `[...]`. If exactly one, build `Expr::Index` as
// before. If more than one, desugar `r[a, b, ...]` to
// `r.get(a, b, ...)` — the existing call-dispatch path handles
// looking up the user's `operator fun get` on the receiver class.
//
// Fix at parser.rs:~2748 (assign side): when the LHS of `=` is a
// `Call(Field(recv, "get"), args)` (produced by the parser
// desugaring above), rewrite as `recv.set(args, rhs)` and emit
// as `Stmt::Expr`. The set call dispatches through the user's
// `operator fun set` on the receiver class.

class Matrix(val rows: Int, val cols: Int) {
    val data: IntArray = IntArray(rows * cols)
    operator fun get(r: Int, c: Int): Int = data[r * cols + c]
    operator fun set(r: Int, c: Int, value: Int) {
        data[r * cols + c] = value
    }
}

fun main() {
    val m = Matrix(2, 3)
    m[0, 0] = 1
    m[0, 1] = 2
    m[1, 2] = 3
    println(m[0, 0])
    println(m[0, 1])
    println(m[1, 2])
    println(m[1, 1])  // default 0
}
