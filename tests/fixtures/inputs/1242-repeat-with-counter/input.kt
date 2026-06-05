// Regression: `repeat(N) { … }` with a zero-param lambda whose body
// doesn't reference `it` must emit a `Function1` lambda — not `Function0`.
// The mir-lowered `repeat` synthesis calls `lambda.invoke(i)`, which
// throws `NoSuchMethodError 'Object.invoke(Object)'` against a Function0.
class Counter(private var n: Int = 0) {
    fun bump(): Int { n++; return n }
    fun value(): Int = n
}

fun main() {
    val c = Counter()
    repeat(3) { c.bump() }
    println(c.value())

    val c2 = Counter()
    repeat(7) { c2.bump() }
    println(c2.value())
}
