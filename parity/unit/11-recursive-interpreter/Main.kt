fun main() {
    // ((3 * 4) + (-5))  →  7
    val e: Expr = Add(Mul(Num(3), Num(4)), Neg(Num(5)))
    println("${e.show()} = ${e.eval()}")
    println(statsOf(e))

    // Deeper nesting: (2 * (3 + 4)) * (-1)
    val e2: Expr = Mul(Mul(Num(2), Add(Num(3), Num(4))), Neg(Num(1)))
    println("${e2.show()} = ${e2.eval()}")
    println(statsOf(e2))
}
