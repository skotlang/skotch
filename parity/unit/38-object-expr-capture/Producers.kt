// Interfaces for `object : I { ... }` to implement. Two interfaces
// with different shapes — string producer (no params, returns String)
// and binary op (two Int params, returns Int) — exercises different
// override signatures and the capture-prelude type machinery for
// both ref and primitive types.

interface Producer {
    fun produce(): String
}

interface BinaryOp {
    fun apply(a: Int, b: Int): Int
}
