// Regression: mutating a `var` instance field then calling a sibling
// method on `this` must NOT use the pre-mutation field value. Before
// the fix, lower_class pre-loaded every field into a local at method
// entry and only wrote back at end-of-body — `inc(); report()` had
// `report` re-read the field and see the old value because the
// `putfield` hadn't run yet.
class Box {
    var n: Int = 0

    fun inc() {
        n += 1
        report()
    }

    fun report() {
        println("box.n = $n")
    }
}

fun main() {
    val b = Box()
    b.inc()
    b.inc()
    b.inc()
}
