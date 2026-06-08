// `if (cond) stmt` without braces where stmt is an ASSIGNMENT (not
// just an expression). Pre-fix parse_branch_block called parse_expr
// for the no-braces case, which can't parse `x = y + 1` (assignment
// is a Stmt, not an Expr). Result: `if (cond) tail = null` failed
// with "expected expression, found Eq" at the `=`.
//
// Fix at parser.rs:~4395 changes the no-braces branch to call
// parse_stmt instead, so Stmt::Assign, Stmt::IndexAssign, and
// Stmt::FieldAssign all parse correctly as single-stmt if-bodies.
// Surfaced by parity/49-functional-pipelines. Uses class-instance
// `var total` (field write-back, supported) rather than a local-
// var-mutating for-loop (separate v0.50 gap that would obscure
// the parser fix).

class Counter {
    var total: Int = 0
    fun maybeBump(x: Int) {
        if (x > 0) total = total + x
    }
    fun maybeReset(x: Int) {
        if (x == 0) total = 0
    }
}

fun main() {
    val c = Counter()
    c.maybeBump(5)
    c.maybeBump(-3)
    c.maybeBump(10)
    println(c.total)
    c.maybeReset(0)
    println(c.total)
    c.maybeBump(42)
    c.maybeReset(7)
    println(c.total)
}
