// Mini state machine with a mutable `var current` and a
// `MutableList<Pair<Light, Event>>` history. Combines features that
// previous examples touched in isolation:
//
//   - generic Pair<Light, Event> stored as elements of a MutableList
//     (Pair stdlib intrinsic + .first/.second accessors)
//   - a public mutable `var` field re-assigned from inside an instance
//     method (the var-field write-through path)
//   - a private helper method whose body is a `when` expression
//     returning Light directly (Unit-vs-value when-return distinction)
//   - nested when: outer match on Event, inner match on Light
//
// `replay(toIndex)` is the sophistication step over example 14: it
// constructs a fresh `Machine`, walks a sub-range of the recorded
// history, and returns the resulting state — so the same transition
// function is exercised twice within one `main()` against two
// independent receivers.
class Machine(start: Light) {
    var current: Light = start
    private val history: MutableList<Pair<Light, Event>> = mutableListOf()

    fun handle(event: Event): Light {
        val prev = current
        val next = transition(current, event)
        history.add(Pair(prev, event))
        current = next
        return next
    }

    fun trace(): List<String> {
        val out = mutableListOf<String>()
        var i = 0
        while (i < history.size) {
            val step = history[i]
            out.add("${i}: ${step.first} --${step.second}-->")
            i++
        }
        return out
    }

    // Replay the first `toIndex` events on a fresh machine, starting
    // from the same initial state this machine had. Returns the
    // resulting current state.
    fun replay(toIndex: Int): Light {
        val initial = if (history.isEmpty()) current else history[0].first
        val fresh = Machine(initial)
        var i = 0
        while (i < toIndex && i < history.size) {
            fresh.handle(history[i].second)
            i++
        }
        return fresh.current
    }

    private fun transition(state: Light, event: Event): Light = when (event) {
        Event.EMERGENCY -> Light.RED
        Event.TICK -> when (state) {
            Light.RED -> Light.GREEN
            Light.GREEN -> Light.YELLOW
            Light.YELLOW -> Light.RED
        }
    }
}
