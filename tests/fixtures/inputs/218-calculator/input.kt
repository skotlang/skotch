fun calculate(a: Int, op: Int, b: Int): Int = when (op) {
    1 -> a + b
    2 -> a - b
    3 -> a * b
    4 -> a / b
    else -> 0
}

fun main() {
    println(calculate(10, 1, 5))
    println(calculate(10, 2, 3))
    println(calculate(6, 3, 7))
    println(calculate(20, 4, 4))
}
