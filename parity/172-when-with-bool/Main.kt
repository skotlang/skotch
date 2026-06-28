fun describeRange(n: Int, lo: Int, hi: Int): String = when {
    n in lo..hi -> "in"
    n < lo -> "below"
    else -> "above"
}

fun main() {
    println(describeRange(5, 1, 10))
    println(describeRange(0, 1, 10))
    println(describeRange(99, 1, 10))
    println(describeRange(10, 1, 10))
    println(describeRange(1, 1, 10))
}
