// Regression: a `when` (or `if-else`) whose branches return UNRELATED
// subtypes of the declared return type must not narrow the result slot
// to the first branch's type — otherwise the backend emits an
// upcast-into-sibling checkcast on every other branch and the program
// fails at runtime with a `ClassCastException` (e.g. casting `Idle`
// into `Running` because `Running` was the first branch). The
// expression result widens to `Ty::Any` once a sibling reference type
// appears so the merge slot tolerates every value.
sealed interface Op {
    fun label(): String
}

object Noop : Op {
    override fun label(): String = "noop"
}

class Add(val v: Int) : Op {
    override fun label(): String = "add($v)"
}

class Sub(val v: Int) : Op {
    override fun label(): String = "sub($v)"
}

fun negate(o: Op): Op = when (o) {
    is Add -> Sub(o.v)        // first branch: Sub
    is Sub -> Add(o.v)        // sibling: Add
    is Noop -> Noop           // sibling: Noop (object singleton)
}

fun pickIfElse(b: Boolean, v: Int): Op =
    if (b) Add(v) else Noop    // sibling-typed if/else, declared Op

fun main() {
    val a: Op = Add(3)
    val b: Op = Sub(7)
    println(negate(a).label())
    println(negate(b).label())
    println(negate(Noop).label())
    println(pickIfElse(true, 9).label())
    println(pickIfElse(false, 9).label())
}
