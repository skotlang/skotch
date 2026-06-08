// User class with `operator fun iterator()` — Kotlin's `for (x in
// receiver)` desugars to:
//   val iter = receiver.iterator()
//   while (iter.hasNext()) { val x = iter.next(); body }
//
// The receiver does NOT have to implement java/lang/Iterable —
// Kotlin's iteration is duck-typed on the operator name. Pre-fix,
// skotch's for-loop unconditionally dispatched `Iterable.iterator()`,
// crashing with IncompatibleClassChangeError on user iterators.

class Range2(val start: Int, val end: Int) {
    operator fun iterator(): Range2Iter = Range2Iter(start, end)
}

// Iterator-protocol class: operator fun hasNext()/next(). Returns
// primitive Int from next() — confirms the for-loop's user-iterator
// path picks up the next() return type from the iterator class's
// MIR method definition and emits the matching descriptor.
class Range2Iter(start: Int, val end: Int) {
    var current: Int = start
    operator fun hasNext(): Boolean = current <= end
    operator fun next(): Int {
        // NOTE: avoid the `val r = current; current = current + 1;
        // return r` pattern — skotch's var-field cache aliases r to
        // current (v0.50 milestone gap). Compute the post-increment
        // return value as `current - 1` instead.
        current = current + 1
        return current - 1
    }
}
