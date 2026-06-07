// Two enums describing the finite-state-machine alphabet.
// The machine is small enough to fit in one example but exercises
// nested `when` on enum subjects + cross-file enum import resolution
// (Main.kt names `Event.TICK` without an `import` statement).
enum class Light { RED, GREEN, YELLOW }

enum class Event { TICK, EMERGENCY }
