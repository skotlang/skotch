// The library file: a generic @JvmInline value class in a MULTI-
// SEGMENT package that imports its opt-in annotations from a nested
// subpackage and applies them to its accessor properties, mirroring
// kotlin-result's Result.kt. (An earlier hypothesis that this layout
// would reproduce 102-result's Main.kt "unresolved reference" errors
// was disproven — those come from the harness compiling Main.kt with
// KOTLINC against skotch's emitted classfiles, which lack the
// @kotlin.Metadata kotlinc needs to see top-level fns and value-class
// type params. What this fixture DOES isolate: the extension getters
// in Getters.kt — declared in a different file than the value class —
// bail with Return(If(Reference, Reference, Reference)).)
package com.example.result

import com.example.result.annotation.UnsafeBoxErrorAccess
import com.example.result.annotation.UnsafeBoxValueAccess

@JvmInline
value class Box<out V, out E> internal constructor(
    private val inlineValue: Any?,
) {

    @Suppress("UNCHECKED_CAST")
    @UnsafeBoxValueAccess
    val value: V
        get() = inlineValue as V

    @Suppress("UNCHECKED_CAST")
    @UnsafeBoxErrorAccess
    val error: E
        get() = (inlineValue as FailureMarker<E>).error

    val isOk: Boolean
        get() = inlineValue !is FailureMarker<*>

    val isErr: Boolean
        get() = inlineValue is FailureMarker<*>
}

private class FailureMarker<out E>(
    val error: E,
)

@Suppress("FunctionName")
fun <V> Ok(value: V): Box<V, Nothing> {
    return Box(value)
}

@Suppress("FunctionName")
fun <E> Err(error: E): Box<Nothing, E> {
    return Box(FailureMarker(error))
}
