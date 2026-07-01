// Cross-file generic utility built on Box. `triplets` returns
// three Boxes with different reference types. Reference-only to
// dodge the cross-file generic-erasure autobox gap: when a cross-
// file generic fn (`fun <A, B> pair(a: A, b: B)`) is invoked with
// primitive args, the call site needs to box the primitive before
// passing to the Object-erased slot — skotch doesn't yet do that
// for cross-file callees (v0.50 milestone gap).

fun triplets(): String {
    val a = pair("x", "1")
    val b = pair("y", "two")
    val c = pair("z", "end")
    return a.toString() + " | " + b.toString() + " | " + c.toString()
}

// A class with a method NAMED like a stdlib extension (`forEach`)
// to confirm that user-class methods win over `CollectionsKt.forEach`.
// If skotch's dispatch is wrong, it'd route Counter.forEach to
// `CollectionsKt.forEach(Iterable, Function1)` and crash with
// ClassCastException at runtime (Counter isn't Iterable).
class Counter(val limit: Int) {
    fun count(): Int = limit
    fun forEach(): String = "iterated ${limit} times"
    fun map(): String = "mapped to ${limit}"
}
