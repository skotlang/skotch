// Custom property delegate with getValue/setValue. Requires
// `kotlin.reflect.KProperty` import + `operator fun getValue` /
// `setValue` with the KProperty signature. Skotch's known status:
// `by lazy { }` works (covered v0.12.5); custom delegate parsing
// works (covered v0.12.5); but `kotlin.reflect.KProperty` may not
// resolve, and the setValue side-effects may not fire correctly.

import kotlin.reflect.KProperty

class Observable<T>(private var value: T) {
    operator fun getValue(thisRef: Any?, prop: KProperty<*>): T {
        return value
    }

    operator fun setValue(thisRef: Any?, prop: KProperty<*>, new: T) {
        value = new
    }
}

class Counter {
    var n: Int by Observable(0)
    var label: String by Observable("counter")
}
