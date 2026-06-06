// Regression: indexed access on a `List<Class>` parameter must emit
// a `checkcast` to the element type so subsequent member access
// resolves virtually. Before the fix, `steps[i].apply(...)` saw the
// `steps[i]` result as `Ty::Any` and lowered to `Const(null)` because
// the Field-lowering couldn't dispatch the method on Any.
interface Action {
    fun run(): Int
}

class IncAction(val by: Int) : Action {
    override fun run(): Int = by + 1
}

fun first(actions: List<Action>): Int {
    return actions[0].run()
}

fun main() {
    val actions: List<Action> = listOf(IncAction(5))
    println(first(actions))
}
