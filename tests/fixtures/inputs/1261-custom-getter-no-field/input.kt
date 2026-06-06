// Regression: a `val x: T get() = ...` property with a custom getter
// and NO initializer must NOT emit a backing field. The synthesized
// `getX()` accessor computes the value on each call. Before the fix
// skotch emitted both the field AND the getter; cross-file callers
// then preferred the field via `getfield x:Object`, mismatching the
// real (non-existent) field's descriptor → NoSuchFieldError.
class Holder {
    private var _value: Int = 0

    val doubled: Int
        get() = _value * 2

    fun bump() {
        _value += 1
    }
}

fun main() {
    val h = Holder()
    println(h.doubled)
    h.bump()
    h.bump()
    println(h.doubled)
}
