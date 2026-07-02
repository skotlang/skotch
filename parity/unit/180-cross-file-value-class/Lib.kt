package foo

@JvmInline
value class Result<out V, out E> internal constructor(private val inline_v: Any?) {
    val isOk: Boolean get() = inline_v !is Failure<*>
}

internal class Failure<out E>(val error: E)
