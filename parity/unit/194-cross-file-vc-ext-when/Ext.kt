// Extensions on the value class declared in a DIFFERENT file than the
// class itself — the cross-file variant of fixture 189's shapes.
@file:OptIn(ExperimentalContracts::class)

package foo

import kotlin.contracts.ExperimentalContracts
import kotlin.contracts.InvocationKind
import kotlin.contracts.contract

// kotlin-result Get.kt getOr — bails Return(When) (13 fns in the
// library share this shape: unwrap/get/getOr/and/or/merge/flatten/...)
fun <V, E> Box<V, E>.unwrapOr(default: V): V {
    return when {
        isOk -> value
        else -> default
    }
}

// kotlin-result And.kt andThen — bails Call(Reference, Lambda) (38 fns
// share this shape: map/mapError/andThen/recover/orElse/zip/...)
inline fun <V, E> Box<V, E>.thenMap(transform: (V) -> Box<V, E>): Box<V, E> {
    contract { callsInPlace(transform, InvocationKind.AT_MOST_ONCE) }
    return when {
        isOk -> transform(value)
        else -> this
    }
}
