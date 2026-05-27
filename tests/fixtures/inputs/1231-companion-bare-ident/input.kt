// Reference a companion-bearing class as a bare Ident expression,
// then pass that value to a function expecting the outer type.
// This mirrors how `Modifier.fillMaxWidth()` evaluates `Modifier` —
// the bare identifier resolves to `Modifier.Companion`, which is
// then used as the receiver of the extension fn.

class Engine {
    companion object {
        fun describe(): String = "engine"
    }
}

object Server {
    fun hello(): String = "hello"
}

fun main() {
    // Bare reference to a class with a companion object: resolves
    // to its `Companion` field, then `.describe()` dispatches on it.
    val e = Engine
    println(e.describe())

    // Bare reference to an `object` declaration: resolves to its
    // `INSTANCE` static field.
    val s = Server
    println(s.hello())
}
