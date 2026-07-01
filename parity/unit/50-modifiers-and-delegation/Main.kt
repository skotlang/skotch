// Drives the delegation + modifier probes. Each section targets a
// distinct gap; if skotch fails on the earlier ones, the later
// ones won't run at all (the parser stops early).

fun main() {
    // ── Property delegation with KProperty getValue/setValue ──
    val c = Counter()
    println(c.n)                    // 0 (initial)
    println(c.label)                // "counter"
    c.n = 42
    c.label = "answer"
    println(c.n)                    // 42
    println(c.label)                // "answer"
    println("---")

    // ── crossinline lambda inside Runnable ──
    var captured = "initial"
    later {
        captured = "ran"
    }
    println(captured)               // "ran" (after Runnable.run)
    println("---")

    // ── noinline lambda passed as a Function param ──
    val doubled = applyOnce({ x -> x * 2 }, 21)
    println(doubled)                // 42
}
