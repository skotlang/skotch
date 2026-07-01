// Isolates the "no type arguments expected for 'class X'" resolver
// failure from parity/full/102-result's Main.kt — the library's
// `@JvmInline value class Result<out V, out E>` has TWO type params,
// but skotch's typeck/resolver seems to only support value classes
// with ZERO or ONE type param and errors when a two-arg annotation
// (`Result<Int, String>`) is applied to a two-param declaration.
// Everything else in Main.kt (getOr, Ok, Err, etc.) then cascades to
// "unresolved reference".
@JvmInline
value class Wrap<out A, out B> internal constructor(private val v: Any?)

@Suppress("FunctionName")
fun <A> Left(value: A): Wrap<A, Nothing> = Wrap(value)

@Suppress("FunctionName")
fun <B> Right(value: B): Wrap<Nothing, B> = Wrap(value)

fun <A, B> Wrap<A, B>.describeSide(): String {
    return "wrap"
}

fun main() {
    val a: Wrap<Int, String> = Left(7)
    val b: Wrap<Int, String> = Right("boom")
    println(a.describeSide())
    println(b.describeSide())
}
