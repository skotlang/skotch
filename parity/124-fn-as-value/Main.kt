fun double(n: Int): Int = n * 2
fun triple(n: Int): Int = n * 3

fun pickOp(useTriple: Boolean): (Int) -> Int = if (useTriple) ::triple else ::double

fun main() {
    val op1 = pickOp(false)
    val op2 = pickOp(true)
    println(op1(7))
    println(op2(7))
    val ops = listOf(::double, ::triple)
    for (op in ops) println(op(5))
}
