// Class delegation via `by`: the decorator pattern. Combines
// overriding (LoudGreeter.greet wins, the auto-forwarder is
// skipped) with synthesized forwarding (LoudGreeter.farewell is
// auto-emitted as `return inner.farewell()`).
//
// Single-file companion to parity/36-class-delegation/, which
// exercises the same pattern split across three files.

interface Greeter {
    fun greet(): String
    fun farewell(): String
}

class FormalGreeter(val name: String) : Greeter {
    override fun greet(): String = "Good day, ${name}."
    override fun farewell(): String = "Goodbye, ${name}."
}

class LoudGreeter(inner: Greeter) : Greeter by inner {
    override fun greet(): String = "GOOD DAY!!!"
}

class QuietGreeter(inner: Greeter) : Greeter by inner

fun main() {
    val formal: Greeter = FormalGreeter("Alice")
    val loud: Greeter = LoudGreeter(formal)
    val quiet: Greeter = QuietGreeter(formal)
    val stacked: Greeter = LoudGreeter(QuietGreeter(formal))
    println(loud.greet())
    println(loud.farewell())
    println(quiet.greet())
    println(quiet.farewell())
    println(stacked.greet())
    println(stacked.farewell())
}
