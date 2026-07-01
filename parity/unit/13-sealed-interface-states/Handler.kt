// `when (s) is X` dispatch over a sealed interface. The exhaustive
// branches cover all three subtypes — typeck verifies the closure
// and emits no `NoWhenBranchMatchedException` fall-through.
fun handle(s: State): String = when (s) {
    is Idle -> "no action — waiting"
    is Running -> "in progress with ${s.task} (${s.progress}%)"
    is Stopped -> "halted because ${s.reason}"
}

// Mutate-and-return helper using a plain class. Returns a NEW
// instance each call (states are immutable here).
fun step(s: State): State = when (s) {
    is Idle -> Running("init", 0)
    is Running -> {
        val next = s.progress + 25
        if (next > 100) Stopped("complete") else Running(s.task, next)
    }
    is Stopped -> Idle
}
