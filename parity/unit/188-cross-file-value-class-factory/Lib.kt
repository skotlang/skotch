package foo

@JvmInline
value class Box<out V, out E> internal constructor(private val inline_v: Any?) {
    val isOk: Boolean get() = inline_v !is FailureMarker<*>
}

internal class FailureMarker<out E>(val error: E)

@Suppress("FunctionName")
fun <V> Ok(value: V): Box<V, Nothing> = Box(value)

@Suppress("FunctionName")
fun <E> Err(error: E): Box<Nothing, E> = Box(FailureMarker(error))
