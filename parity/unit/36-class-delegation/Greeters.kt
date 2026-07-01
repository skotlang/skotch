// Concrete impl + a delegating decorator.
//
// `LoudGreeter(inner: Greeter) : Greeter by inner` is the test of
// interest. Kotlin must:
//   1. Store `inner` as a synthetic field (the param isn't `val`).
//   2. Synthesize `farewell()` as a forwarder: `return inner.farewell()`.
//   3. NOT synthesize `greet()` because LoudGreeter overrides it
//      explicitly — the override wins, the auto-forwarder is skipped.
//
// QuietGreeter shows the same pattern with a different override —
// covers the case where the delegating class wraps the result instead
// of bypassing the forwarder.

class FormalGreeter(val name: String) : Greeter {
    override fun greet(): String = "Good day, ${name}."
    override fun farewell(): String = "Goodbye, ${name}."
}

class LoudGreeter(inner: Greeter) : Greeter by inner {
    // greet() is overridden — the inner.greet() forwarder is NOT
    // synthesized for this method.
    override fun greet(): String = "GOOD DAY!!!"
    // farewell() is inherited via `by inner` — the synthesized
    // forwarder calls inner.farewell().
}

class QuietGreeter(inner: Greeter) : Greeter by inner {
    // Both methods are delegated through `by inner` — neither is
    // overridden. The decorator simply passes through.
}
