// Regression: a class implementing two unrelated interfaces — the
// emitted class file must list both in the `interfaces` table, and
// virtual dispatch through either interface reference must work.
interface Greeter {
    fun greet(): String
}

interface Counter {
    fun next(): Int
}

class TaggedCounter(private val tag: String) : Greeter, Counter {
    private var n: Int = 0

    override fun greet(): String = "hello from $tag"

    override fun next(): Int {
        n += 1
        return n
    }
}

fun main() {
    val tc = TaggedCounter("alpha")
    val g: Greeter = tc
    val c: Counter = tc
    println(g.greet())
    println(c.next())
    println(c.next())
    println(g.greet())
}
