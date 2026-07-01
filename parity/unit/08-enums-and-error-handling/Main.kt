fun main() {
    // Use a list of (a, op, b) triples to avoid string parsing.
    println("3 + 4 = ${Calculator.evaluate(3, Op.PLUS, 4)}")
    println("10 - 7 = ${Calculator.evaluate(10, Op.MINUS, 7)}")
    println("6 * 8 = ${Calculator.evaluate(6, Op.TIMES, 8)}")
    println("20 / 4 = ${Calculator.evaluate(20, Op.DIV, 4)}")

    try {
        println("10 / 0 = ${Calculator.evaluate(10, Op.DIV, 0)}")
    } catch (ex: CalcError) {
        println("10 / 0 -> calc error: ${ex.message}")
    }
}
