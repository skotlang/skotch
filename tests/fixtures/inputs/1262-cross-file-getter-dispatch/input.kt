// Regression: cross-file property access `obj.prop` must dispatch
// through the synthesized `getProp()` accessor — kotlinc emits
// `invokevirtual Foo.getX()T` for every Kotlin property reference
// outside the declaring file. skotch-resolve now surfaces property
// getters in the cross-file `ExternalClassDecl.methods` list so the
// Field-lowering's getter-method check finds them. Without this,
// callers fell through to `getfield` with a fabricated `Object`
// descriptor → NoSuchFieldError. Single-file fixture for now; the
// real cross-file shape is exercised by example 10.
class Holder(val value: Int) {
    val doubled: Int
        get() = value * 2
}

fun main() {
    val h = Holder(5)
    println(h.value)
    println(h.doubled)
}
