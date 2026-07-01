// Isolates the `Call(Reference, Lambda)` bail from parity/full/102-result
// — the most common (38 occurrences). The library's inline extension
// functions open their body with a `contract { callsInPlace(fn, …) }`
// call, then do the real work. skotch's mir-lower typed body walker
// sees `contract { … }` as its first statement, bails on the shape
// (Call+Lambda), and drops the entire function body. Fix: recognize
// `kotlin.contracts.contract { … }` as a compile-time-only DSL and
// skip it (no bytecode emitted for it — kotlinc's compiled shape has
// nothing there either), then continue lowering the rest of the body.
@file:OptIn(ExperimentalContracts::class)

import kotlin.contracts.contract
import kotlin.contracts.InvocationKind
import kotlin.contracts.ExperimentalContracts

inline fun apply2(fn: () -> Int): Int {
    contract { callsInPlace(fn, InvocationKind.EXACTLY_ONCE) }
    return fn()
}

inline fun mapInt(x: Int, f: (Int) -> Int): Int {
    contract { callsInPlace(f, InvocationKind.EXACTLY_ONCE) }
    return f(x)
}

fun main() {
    println(apply2 { 42 })
    println(mapInt(5) { it * 3 })
    println(mapInt(-2) { it + 100 })
}
