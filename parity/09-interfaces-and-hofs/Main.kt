// Higher-order function taking a `(Int, Step) -> Unit` callback —
// the lambda has TWO non-primitive params (Int + interface
// reference) and is called once per step. Returns the final value.
fun runPipeline(initial: Int, steps: List<Step>, onEach: (Int, Step) -> Unit): Int {
    var current = initial
    var i = 0
    while (i < steps.size) {
        val step = steps[i]
        current = step.apply(current)
        onEach(current, step)
        i++
    }
    return current
}

fun main() {
    val pipeline: List<Step> = listOf(
        AddStep(5),
        MultiplyStep(3),
        AddStep(-2),
        NegateStep(),
    )
    val result = runPipeline(10, pipeline) { value, step ->
        println("${step.describe()} -> $value")
    }
    println("final: $result")
}
