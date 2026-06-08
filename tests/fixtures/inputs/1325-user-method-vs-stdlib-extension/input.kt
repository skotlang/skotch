// User class with methods NAMED like stdlib extensions (`forEach`,
// `map`). Pre-fix: skotch's method dispatch for `c.forEach()`
// would route through `dynamic_stdlib_extension("Counter",
// "forEach")`. That helper's fallback at lib.rs:~560 maps unknown
// classes to `Iterable`/`Object` receiver descriptors, so any user
// class with a method matching a known stdlib extension would
// silently route to `CollectionsKt.forEach(Iterable, Function1)`
// — a `ClassCastException: Counter cannot be cast to Iterable`
// at runtime (or `IncompatibleClassChangeError` for stable values
// of the lambda machinery).
//
// Fix at lib.rs:~11232: gate the dynamic_stdlib_extension dispatch
// on `!user_class_has_method`. The user-class lookup at lib.rs:~10822
// already runs first and sets the flag — we now use it to skip
// the stdlib fallback so the Virtual dispatch below wins.

class Counter(val limit: Int) {
    fun count(): Int = limit
    fun forEach(): String = "iter:${limit}"
    fun map(): String = "map:${limit}"
}

fun main() {
    val c = Counter(7)
    println(c.count())
    println(c.forEach())
    println(c.map())
}
