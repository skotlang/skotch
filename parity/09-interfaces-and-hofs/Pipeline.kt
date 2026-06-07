// Interface with abstract methods + a default-bodied method that
// concrete implementations can either inherit OR override. Java
// compiles this to a regular interface with a `default` method;
// kotlinc emits the body inside a `DefaultImpls` inner class and
// adds a synthetic forwarding method on each implementor (since
// Kotlin defaults to JVM target without Java 8 default methods).
interface Step {
    fun name(): String
    fun apply(input: Int): Int
    fun describe(): String = "step[${name()}]"
}

class AddStep(val amount: Int) : Step {
    override fun name(): String = "add($amount)"
    override fun apply(input: Int): Int = input + amount
}

class MultiplyStep(val factor: Int) : Step {
    override fun name(): String = "mul($factor)"
    override fun apply(input: Int): Int = input * factor
}

class NegateStep : Step {
    override fun name(): String = "neg"
    override fun apply(input: Int): Int = -input
    // Override the interface's default method for one concrete impl.
    override fun describe(): String = "[neg-step]"
}
