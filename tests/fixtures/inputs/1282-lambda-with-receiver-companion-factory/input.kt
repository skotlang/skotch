// Regression: a companion-object factory taking a
// `Receiver.() -> Unit` lambda. The call site needs to plumb the
// receiver type through to the lambda's invoke method so bare-name
// method calls (`add(...)` here) inside the lambda body resolve as
// `this.add(...)`. Pre-fix, skotch returned an `IncompatibleClass`
// at runtime because the lambda implemented Function0 instead of
// Function1<Receiver, Unit>.
class Box {
    var sum: Int = 0
    fun add(x: Int) { sum += x }

    companion object {
        fun build(init: Box.() -> Unit): Box {
            val b = Box()
            b.init()
            return b
        }
    }
}

fun main() {
    val b = Box.build {
        add(1)
        add(2)
        add(3)
    }
    println(b.sum)
}
