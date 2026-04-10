fun classify(n: Int): String {
    return if (n < 0) {
        "negative"
    } else if (n == 0) {
        "zero"
    } else {
        "positive"
    }
}

fun main() {
    println(classify(-5))
    println(classify(0))
    println(classify(42))
}
