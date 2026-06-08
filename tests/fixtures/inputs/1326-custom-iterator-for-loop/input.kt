// User-defined iterator protocol: `operator fun iterator()` on a
// class that is NOT java/lang/Iterable. Kotlin's `for` loop
// desugars to dispatching the iterator + hasNext()/next() methods
// duck-typed on the operator names — the receiver does NOT need
// to implement Iterable.
//
// Pre-fix: skotch's for-loop lowering unconditionally emitted
// `invokevirtual java/lang/Iterable.iterator()` regardless of
// receiver type → `IncompatibleClassChangeError: Range2 does
// not implement java.lang.Iterable` at runtime.
//
// Fix at mir-lower:~5772 detects user classes with `operator fun
// iterator()` and dispatches through the user method + reads the
// returned iterator class for the hasNext/next dispatch targets +
// reads next()'s return type from the iterator class's MIR
// method definition (so primitives return I not Object).

class Range2Iter(start: Int, val end: Int) {
    var current: Int = start
    operator fun hasNext(): Boolean = current <= end
    operator fun next(): Int {
        current = current + 1
        return current - 1
    }
}

class Range2(val start: Int, val end: Int) {
    operator fun iterator(): Range2Iter = Range2Iter(start, end)
}

fun main() {
    for (i in Range2(1, 5)) {
        println(i)
    }
    var sum = 0
    for (i in Range2(10, 15)) {
        sum = sum + i
    }
    println(sum)
}
