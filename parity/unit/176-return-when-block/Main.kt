// Isolates the `Return(When)` bail from parity/full/102-result — 13
// library functions produce empty MIR because their body is
// `{ return when { … } }` (block body, explicit `return`, when as the
// returned expression). kotlinc happily emits the block+when; skotch's
// mir-lower typed body walker gives up on the Return(WhenExpr) shape.
// The expression-body form `fun f(): T = when { … }` (fixture 172)
// already works.
fun describe(n: Int): String {
    return when {
        n < 0 -> "neg"
        n == 0 -> "zero"
        else -> "pos"
    }
}

fun bucket(n: Int): String {
    return when {
        n < 10 -> "small"
        n < 100 -> "medium"
        else -> "large"
    }
}

fun main() {
    println(describe(-5))
    println(describe(0))
    println(describe(7))
    println(bucket(3))
    println(bucket(42))
    println(bucket(500))
}
