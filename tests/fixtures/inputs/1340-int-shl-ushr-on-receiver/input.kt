// Lock-in for parity/101-hash work: `Int.shl(Int)` and `Int.ushr(Int)`
// must lower to the JVM `ishl`/`iushr` opcodes rather than dispatch to
// a non-existent `kotlin/Int` class. Both explicit-receiver and
// implicit-receiver (inside an `Int.<ext>()` body, where `this:Int`)
// must work — the bitwise intrinsic table is consulted from both call
// paths.
fun explicit(n: Int): Int {
    return n.shl(3)
}

fun Int.implicit(): Int {
    return shl(3) + ushr(29)
}

fun main() {
    println(explicit(5))
    println(7.implicit())
}
