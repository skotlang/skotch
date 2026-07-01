// Interface with two abstract methods. Both lack default
// implementations on purpose — that exercises the synthesized
// forwarding methods on the `by`-delegating class, since they must
// be emitted (no default to fall back on).

interface Greeter {
    fun greet(): String
    fun farewell(): String
}
