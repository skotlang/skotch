// Isolates the `Property(...)` bail family hit by 6 kotlin-result
// functions — inline generic HOF extensions whose FIRST statement is a
// mundane local declaration that mir-lower nonetheless gives up on:
//
//   tryFold / tryFoldRight   → `var accumulator = initial`
//                              bails Property(Reference)
//   tryReduce / tryReduceIndexed → `val iterator = iterator()`
//                              bails Property(Call(Reference))
//   tryFindLast              → `var last: T? = null`
//                              bails Property(Null)
//   collectionSizeOrDefault  → `return if (this is Collection<*>) size else default`
//                              bails Return(If(Is, Reference, Reference))
//
// Mirror of kotlin-result Try.kt tryFold / tryReduce / tryFindLast /
// collectionSizeOrDefault.

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

fun <T> Iterable<T>.collectionSizeOrDefault(default: Int): Int {
    return if (this is Collection<*>) size else default
}

inline fun <T, R, E> Iterable<T>.tryFold(
    initial: R,
    operation: (acc: R, T) -> Box<R, E>,
): Box<R, E> {
    var accumulator = initial

    for (element in this) {
        val result = operation(accumulator, element)

        accumulator = when {
            result.isOk -> result.value
            else -> return Err(result.error)
        }
    }

    return Ok(accumulator)
}

inline fun <S, T : S, E> Iterable<T>.tryReduce(
    operation: (acc: S, T) -> Box<S, E>,
): Box<S, E>? {
    val iterator = iterator()

    if (!iterator.hasNext()) {
        return null
    }

    var accumulator: S = iterator.next()

    while (iterator.hasNext()) {
        val result = operation(accumulator, iterator.next())

        accumulator = when {
            result.isOk -> result.value
            else -> return Err(result.error)
        }
    }

    return Ok(accumulator)
}

inline fun <T, E> Iterable<T>.tryFindLast(
    predicate: (T) -> Box<Boolean, E>,
): Box<T, E>? {
    var last: T? = null
    var found = false

    for (element in this) {
        val result = predicate(element)

        when {
            result.isErr -> return Err(result.error)
            result.value == true -> {
                last = element
                found = true
            }
        }
    }

    @Suppress("UNCHECKED_CAST")
    return if (found) {
        Ok(last as T)
    } else {
        null
    }
}

fun main() {
    val nums = listOf(1, 2, 3, 4)

    println(nums.collectionSizeOrDefault(99))

    val sum = nums.tryFold(0) { acc, n -> Ok(acc + n) }
    println(sum.isOk)
    println(sum.value)

    val reduced = nums.tryReduce { acc, n -> Ok(acc * n) }
    println(reduced?.isOk)
    println(reduced?.value)

    val lastEven = nums.tryFindLast { n -> Ok(n % 2 == 0) }
    println(lastEven?.value)

    val failed = nums.tryFold(0) { _, n ->
        if (n < 3) Ok(n) else Err("too big: $n")
    }
    println(failed.isErr)
    println(failed.error)
}
