// Bound member references: `obj::method` produces a function value
// bound to the receiver — `s::length` is equivalent to `{ -> s.length }`.
// Also: class member references like `String::length` produce a
// function with an explicit receiver param: `(String) -> Int`.
//
// Skotch status (suspected): callable references for top-level fns
// might work; bound member references on instances likely don't —
// they require synthesizing an inner lambda class that captures the
// receiver.

class Counter(private var n: Int) {
    fun inc() {
        n += 1
    }
    fun value(): Int = n
}

fun applyTimes(times: Int, action: () -> Unit) {
    var i = 0
    while (i < times) {
        action()
        i += 1
    }
}

fun mapAll(items: List<String>, f: (String) -> Int): List<Int> {
    val out = mutableListOf<Int>()
    for (it in items) {
        out.add(f(it))
    }
    return out
}
