fun classify(x: Int): String = when {
    x < 0 -> when {
        x < -100 -> "very negative"
        else -> "negative"
    }
    x == 0 -> "zero"
    else -> "positive"
}

fun main() {
    println(classify(-500))
    println(classify(-5))
    println(classify(0))
    println(classify(42))
}
