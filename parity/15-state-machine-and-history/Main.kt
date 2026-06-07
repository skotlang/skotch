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

    // Replay the first two recorded events on a fresh machine and
    // confirm we arrive at YELLOW (RED → GREEN → YELLOW).
    val rewound = m.replay(2)
    println("rewound to step 2: ${rewound}")

    // Replay everything except the last event — same result as the
    // machine's current state one step ago.
    val almost = m.replay(4)
    println("rewound to step 4: ${almost}")
}
