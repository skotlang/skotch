// Generic inline-function pipelines + var-capture in lambdas
// (var-capture Ref-boxing is a known v0.50 gap; without it the
// closure sees a snapshot, not the live var).

inline fun <T> Iterable<T>.countWhere(predicate: (T) -> Boolean): Int {
    var n = 0
    for (item in this) {
        if (predicate(item)) n = n + 1
    }
    return n
}

inline fun <T, R> Iterable<T>.foldMap(initial: R, op: (R, T) -> R): R {
    var acc = initial
    for (item in this) {
        acc = op(acc, item)
    }
    return acc
}

// Reified inline + cross-file. Returns the FIRST element of the
// given type, or null if none. Probes inline + reified across files
// (single-reified is "supported" per milestones; cross-file
// dispatch may surface issues).
inline fun <reified T> Iterable<*>.firstOfType(): T? {
    for (item in this) {
        if (item is T) return item
    }
    return null
}
