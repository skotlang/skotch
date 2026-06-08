// Generic class with two type params + non-lambda methods (`swap`,
// `pair`, comparison helpers). Probes:
//   - generic type-param substitution (Box<A, B>.swap() returns
//     Box<B, A> — A/B positions flip).
//   - user-class methods named like stdlib extensions (`get`, `pair`,
//     `swap`) — must dispatch to the user method, not stdlib's
//     Iterable/Map extensions.
//   - cross-file generic class instantiation + method dispatch.
//
// Methods take ONLY data params (no lambdas). Generic class methods
// that take `(T) -> R` parameters trip a separate lambda-bridge gap
// (Function1 interface not implemented by skotch's lambda classes
// when the receiver is a generic-erased user-class method) — that
// gap is documented in milestones; this example stays clear of it.

class Box<A, B>(val first: A, val second: B) {
    fun swap(): Box<B, A> = Box(second, first)
    fun firstOnly(): A = first
    fun secondOnly(): B = second
    override fun toString(): String = "(${first}, ${second})"
}

fun <A, B> pair(a: A, b: B): Box<A, B> = Box(a, b)

fun <A, B> firstOf(b: Box<A, B>): A = b.firstOnly()
