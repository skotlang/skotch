// `sealed interface` (Kotlin 1.5+) — a closed hierarchy of types
// where every direct subtype must be declared in the same module.
// Unlike `sealed class`, sealed interfaces can be implemented by
// classes that already extend something else and can be implemented
// by object singletons. The `when` exhaustiveness check uses the
// closed subtype list to verify all branches.
sealed interface State {
    fun describe(): String
}

// Object singleton as a sealed-interface subtype — no constructor,
// one INSTANCE, polymorphic dispatch through the interface.
object Idle : State {
    override fun describe(): String = "idle"
}

class Running(val task: String, val progress: Int) : State {
    override fun describe(): String = "running '$task' at $progress%"
}

class Stopped(val reason: String) : State {
    override fun describe(): String = "stopped: $reason"
}
