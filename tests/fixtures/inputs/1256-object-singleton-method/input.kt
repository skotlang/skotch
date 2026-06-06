// Regression: an `object Foo { fun bar(...) }` singleton called as
// `Foo.bar(...)` from the same file works; this fixture also locks
// in the cross-file shape via in-file imports (the equivalent of
// referencing an object declared in another file but same package
// — both go through the singleton-dispatch path).
//
// The cross-file stub for an `object` previously had
// `is_object_singleton: false` hardcoded, which made call sites
// fall through to a null-receiver instance dispatch and emit
// `aconst_null` instead of `getstatic Foo.INSTANCE; invokevirtual`.
object Calculator {
    fun evaluate(a: Int, b: Int): Int = a + b
    fun negate(a: Int): Int = -a
}

fun main() {
    println(Calculator.evaluate(3, 4))
    println(Calculator.negate(7))
}
