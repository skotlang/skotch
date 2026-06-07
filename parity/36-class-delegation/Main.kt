// Drives the delegation patterns.
//
// `loud` proves that LoudGreeter's explicit `greet()` override is
// invoked (returns "GOOD DAY!!!"), while `loud.farewell()` falls
// through to the synthesized forwarder → inner.farewell().
//
// `quiet` proves the all-delegated path: every method goes through
// the synthesized forwarder.

fun main() {
    val formal: Greeter = FormalGreeter("Alice")
    println(formal.greet())          // "Good day, Alice."
    println(formal.farewell())       // "Goodbye, Alice."

    val loud: Greeter = LoudGreeter(formal)
    println(loud.greet())            // "GOOD DAY!!!" — override
    println(loud.farewell())         // "Goodbye, Alice." — forwarded

    val quiet: Greeter = QuietGreeter(formal)
    println(quiet.greet())           // "Good day, Alice." — forwarded
    println(quiet.farewell())        // "Goodbye, Alice." — forwarded

    // Stack two layers: LoudGreeter wrapping QuietGreeter wrapping
    // FormalGreeter. Confirms the delegate chain forwards through
    // multiple levels.
    val stacked: Greeter = LoudGreeter(QuietGreeter(formal))
    println(stacked.greet())         // "GOOD DAY!!!"
    println(stacked.farewell())      // "Goodbye, Alice." — through 2 forwarders
}
