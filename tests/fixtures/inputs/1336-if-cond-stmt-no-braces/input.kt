// `if (cond) stmt` without braces — single-statement branch where
// `stmt` is an assignment (not just an expression). Pre-fix the
// parse_branch_block helper at parser.rs:~4395 called parse_expr
// for the no-braces case, which can't parse `x = y + 1` (assignment
// is a Stmt, not an Expr). Result: `if (cond) n = n + 1` failed
// with "expected expression, found Eq" at the `=`.
//
// Fix changes the no-braces branch to call parse_stmt instead, so
// Stmt::Assign, Stmt::IndexAssign, and Stmt::FieldAssign all
// parse correctly as single-stmt if-bodies. Surfaced by
// parity/49-functional-pipelines.

fun count(items: List<Int>, threshold: Int): Int {
    var n = 0
    for (item in items) {
        if (item > threshold) n = n + 1
    }
    return n
}

class Counter {
    var total: Int = 0
    fun maybeBump(x: Int) {
        if (x > 0) total = total + x
    }
}

fun main() {
    val nums = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    println(count(nums, 5))
    val c = Counter()
    c.maybeBump(5)
    c.maybeBump(-3)
    c.maybeBump(10)
    println(c.total)
}
