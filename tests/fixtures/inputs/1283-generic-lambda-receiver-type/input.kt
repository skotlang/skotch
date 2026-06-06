// Regression: parsing `Box<T>.() -> Unit` (lambda-with-receiver
// whose receiver is a generic class). The bare `Type.()` form
// already worked; this fixture locks in the post-type-args path so
// the parser sees `<T>` AND `.()` and emits the right receiver-typed
// function type.
class Box<T>(val value: T) {
    var sum: Int = 0
    fun add(x: Int) { sum += x }
}

fun <T> build(init: Box<T>.() -> Unit, init_value: T): Box<T> {
    val b = Box(init_value)
    b.init()
    return b
}

fun main() {
    val b = build<Int>({ add(1); add(2); add(3) }, 0)
    println(b.sum)
}
