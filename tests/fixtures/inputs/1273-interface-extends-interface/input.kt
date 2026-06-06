// Regression for audit finding #8: interfaces can extend other
// interfaces. The parser used to silently drop the `: Parent` clause
// on `interface Child : Parent`, and `InterfaceDecl` had no
// `interfaces` field, so dispatching across the inheritance chain
// failed at runtime.
interface Greeter {
    fun greet(): String
}

interface FormalGreeter : Greeter {
    fun salutation(): String

    fun formal(): String = "${salutation()}, ${greet()}"
}

class Englishman : FormalGreeter {
    override fun greet(): String = "good day"
    override fun salutation(): String = "Dear Sir/Madam"
}

fun main() {
    val e: FormalGreeter = Englishman()
    println(e.greet())
    println(e.salutation())
    println(e.formal())
    // Dispatch through the parent interface reference too.
    val g: Greeter = e
    println(g.greet())
}
