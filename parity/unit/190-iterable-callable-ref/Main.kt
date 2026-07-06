// Isolates the `Return(Call(Reference, CallableRef))` bail hit by 6
// kotlin-result functions (allOk/allErr/anyOk/anyErr/countOk/countErr)
// — an extension fn on `Iterable<Result<V, E>>` whose body forwards to
// a stdlib HOF (`all`/`any`/`count`) with a BOUND PROPERTY REFERENCE
// on the generic value class as the predicate.
//
// Mirror of kotlin-result Iterable.kt:
//   public fun <V, E> Iterable<Result<V, E>>.allOk(): Boolean {
//       return all(Result<V, E>::isOk)
//   }

@JvmInline
value class Box<out V, out E> internal constructor(private val inlineV: Any?) {
    val isOk: Boolean get() = inlineV !is FailureMarker<*>
    val isErr: Boolean get() = inlineV is FailureMarker<*>
}

internal class FailureMarker<out E>(val error: E)

@Suppress("FunctionName")
fun <V> Ok(value: V): Box<V, Nothing> = Box(value)

@Suppress("FunctionName")
fun <E> Err(error: E): Box<Nothing, E> = Box(FailureMarker(error))

fun <V, E> Iterable<Box<V, E>>.allOk(): Boolean {
    return all(Box<V, E>::isOk)
}

fun <V, E> Iterable<Box<V, E>>.anyErr(): Boolean {
    return any(Box<V, E>::isErr)
}

fun <V, E> Iterable<Box<V, E>>.countOk(): Int {
    return count(Box<V, E>::isOk)
}

fun main() {
    val mixed: List<Box<Int, String>> = listOf(Ok(1), Err("x"), Ok(3))
    val allGood: List<Box<Int, String>> = listOf(Ok(1), Ok(2))
    println(mixed.allOk())
    println(allGood.allOk())
    println(mixed.anyErr())
    println(allGood.anyErr())
    println(mixed.countOk())
    println(allGood.countOk())
}
