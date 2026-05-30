// A companion-object method annotated `@JvmStatic` should be callable
// directly as a static method on the outer class. Without the static
// delegate, `Engine.describe()` resolves to a static call on `Engine`
// rather than going through `Engine.Companion.INSTANCE.describe()`,
// and either misses entirely or runs the wrong code path.

class Engine {
    companion object {
        @JvmStatic
        fun describe(): String = "engine v2"
    }
}

fun main() {
    println(Engine.describe())
}
