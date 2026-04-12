fun abs(x: Int): Int {
    if (x < 0) return -x
    return x
}

fun sign(x: Int): Int {
    if (x > 0) return 1
    if (x < 0) return -1
    return 0
}

fun main() {
    println(abs(-42))
    println(abs(42))
    println(sign(10))
    println(sign(-5))
    println(sign(0))
}
