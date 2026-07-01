typealias IntOp = (Int, Int) -> Int

fun apply(a: Int, b: Int, op: IntOp): Int = op(a, b)

val add: IntOp = { x, y -> x + y }
val mul: IntOp = { x, y -> x * y }

fun main() {
    println(apply(3, 4, add))
    println(apply(3, 4, mul))
    println(apply(7, 11, { x, y -> x - y }))
}
