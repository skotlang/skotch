// The value class lives HERE; the extensions live in Ext.kt. That
// split is the whole point of this fixture — see Main.kt.
package foo

@JvmInline
value class Box<out V, out E> internal constructor(private val inlineV: Any?) {
    @Suppress("UNCHECKED_CAST")
    val value: V get() = inlineV as V
    @Suppress("UNCHECKED_CAST")
    val error: E get() = (inlineV as FailureMarker<E>).error
    val isOk: Boolean get() = inlineV !is FailureMarker<*>
    val isErr: Boolean get() = inlineV is FailureMarker<*>
}

internal class FailureMarker<out E>(val error: E)

@Suppress("FunctionName")
fun <V> Ok(value: V): Box<V, Nothing> = Box(value)

@Suppress("FunctionName")
fun <E> Err(error: E): Box<Nothing, E> = Box(FailureMarker(error))
