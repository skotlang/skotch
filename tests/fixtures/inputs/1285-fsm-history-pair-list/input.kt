// Mini finite-state-machine that records its event history as a
// `MutableList<Pair<Light, Event>>` and supports replay onto a fresh
// instance. Combines:
//   - two enums with three- and two-entry shapes
//   - var-field re-assignment from inside an instance method
//   - mutableListOf<Pair<X, Y>> with index-based reads + .first/.second
//   - nested when expression returning Light
//   - constructing a fresh receiver of the same class from a method
//   - cross-method dispatch between handle()/replay() over the history
enum class Light { RED, GREEN, YELLOW }

enum class Event { TICK, EMERGENCY }

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

fun main() {
    val m = Machine(Light.RED)
    val events = listOf(
        Event.TICK,
        Event.TICK,
        Event.EMERGENCY,
        Event.TICK,
        Event.TICK
    )
    for (e in events) {
        val to = m.handle(e)
        println("now: ${to}")
    }

    println("trace:")
    for (line in m.trace()) println(line)

    val rewound = m.replay(2)
    println("rewound to step 2: ${rewound}")

    val almost = m.replay(4)
    println("rewound to step 4: ${almost}")
}
