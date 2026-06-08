// Anonymous `object : I { ... }` expressions that capture local
// vals from the enclosing scope. Pre-fix: the synthesized anonymous
// class had no fields, the constructor took no args, and method
// bodies looked up captures in an empty scope → resolved to null
// (for refs) or 0 (for primitives). Result: `produce()` returning
// `prefix + suffix` always printed `null` instead of the captured
// strings.
//
// Fix (single-file version of the parity/38 multi-file probe):
// `Expr::ObjectExpr` lowering at lib.rs:~21574 now calls
// collect_free_vars across all method bodies, adds each capture as
// a class field, takes them as constructor params (stored via
// putfield after super()), threads them from the outer scope at
// the instantiation site, and emits a per-method prelude that
// GetFields each capture into a local so body references resolve.

interface Producer {
    fun produce(): String
}

interface BinaryOp {
    fun apply(a: Int, b: Int): Int
}

fun main() {
    val tag = "[ok]"
    val suffix = " — done"
    val producer = object : Producer {
        override fun produce(): String = tag + suffix
    }
    println(producer.produce())

    val k = 100
    val op = object : BinaryOp {
        override fun apply(a: Int, b: Int): Int = a * b + k
    }
    println(op.apply(3, 4))
}
