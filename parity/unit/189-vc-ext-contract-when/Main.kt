// Isolates the specific `Call(Reference, Lambda)` bail on 38 kotlin-
// result library functions — extension fn on a `@JvmInline value class`
// whose body opens with `contract { … }` and then a `return when { … }`
// referencing receiver members. My earlier 177 fixture covered the
// contract-DSL at the top of a plain fn body; the library shape adds a
// value-class receiver + a receiver-member-reading `when`, which the
// mir-lower body walker still gives up on.
//
// Mirror of kotlin-result:
//   public inline infix fun <V, E, U> Result<V, E>.andThen(
//       transform: (V) -> Result<U, E>,
//   ): Result<U, E> {
//       contract { callsInPlace(transform, InvocationKind.AT_MOST_ONCE) }
//       return when {
//           isOk -> transform(value)
//           else -> this.asErr()
//       }
//   }
@file:OptIn(ExperimentalContracts::class)

import kotlin.contracts.contract
import kotlin.contracts.InvocationKind
import kotlin.contracts.ExperimentalContracts

@JvmInline
value class Box internal constructor(private val v: Any?) {
    val isOk: Boolean get() = v != null
    val value: Any? get() = v
}

fun makeOk(x: Int): Box = Box(x)
fun makeNone(): Box = Box(null)

inline fun Box.thenMap(transform: (Any?) -> Box): Box {
    contract { callsInPlace(transform, InvocationKind.AT_MOST_ONCE) }
    return when {
        isOk -> transform(value)
        else -> this
    }
}

fun main() {
    val ok = makeOk(7)
    val none = makeNone()
    println(ok.isOk)
    println(none.isOk)
    val mapped = ok.thenMap { Box((it as Int) * 2) }
    val skipped = none.thenMap { Box(-1) }
    println(mapped.isOk)
    println(skipped.isOk)
    println(mapped.value)
}
