// Isolates the `Return(Call(Reference, Call, Reference))` bail hit by
// 11 kotlin-result functions (tryFilter/tryAssociate/tryGroupBy/...)
// plus the sibling shapes `Return(Call(Reference, Call))` (filterOk,
// filterErr) and `Return(Call(Reference, Call, Call))` (partition,
// tryPartition) — an inline extension fn whose whole body forwards to
// a sibling `...To(destination, ...)` extension, passing a freshly
// constructed `ArrayList()` plus the lambda parameter through.
//
// Mirror of kotlin-result Try.kt / Iterable.kt:
//   public inline fun <T, E> Iterable<T>.tryFilter(
//       predicate: (T) -> Result<Boolean, E>,
//   ): Result<List<T>, E> {
//       return tryFilterTo(ArrayList(), predicate)
//   }
//   public fun <V, E> Iterable<Result<V, E>>.filterOk(): List<V> {
//       return filterOkTo(ArrayList())
//   }
//   public fun <V, E> Iterable<Result<V, E>>.partition(): Pair<...> {
//       return partitionTo(ArrayList(), ArrayList())
//   }

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

inline fun <T, E> Iterable<T>.tryFilter(
    predicate: (T) -> Box<Boolean, E>,
): Box<List<T>, E> {
    return tryFilterTo(ArrayList(), predicate)
}

inline fun <T, E, C : MutableCollection<in T>> Iterable<T>.tryFilterTo(
    destination: C,
    predicate: (T) -> Box<Boolean, E>,
): Box<C, E> {
    for (element in this) {
        val result = predicate(element)

        when {
            result.isErr -> return Err(result.error)
            result.value -> destination.add(element)
        }
    }
    return Ok(destination)
}

fun <V, E> Iterable<Box<V, E>>.filterOk(): List<V> {
    return filterOkTo(ArrayList())
}

fun <V, E, C : MutableCollection<in V>> Iterable<Box<V, E>>.filterOkTo(destination: C): C {
    for (element in this) {
        if (element.isOk) {
            destination.add(element.value)
        }
    }
    return destination
}

fun main() {
    val nums = listOf(1, 2, 3, 4, 5)
    val evens = nums.tryFilter { n -> Ok(n % 2 == 0) }
    println(evens.isOk)
    println(evens.value)

    val mixed: List<Box<Int, String>> = listOf(Ok(10), Err("bad"), Ok(30))
    println(mixed.filterOk())
}
