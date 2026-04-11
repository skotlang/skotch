fun classify(x: Int, y: Int): String = when {
    x > 0 && y > 0 -> "Q1"
    x < 0 && y > 0 -> "Q2"
    x < 0 && y < 0 -> "Q3"
    x > 0 && y < 0 -> "Q4"
    else -> "axis"
}

fun main() {
    println(classify(1, 1))
    println(classify(-1, 1))
    println(classify(-1, -1))
    println(classify(1, -1))
    println(classify(0, 5))
}
