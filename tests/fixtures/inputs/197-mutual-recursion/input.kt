fun isEven(n: Int): Boolean {
    if (n == 0) { return true }
    return isOdd(n - 1)
}

fun isOdd(n: Int): Boolean {
    if (n == 0) { return false }
    return isEven(n - 1)
}

fun main() {
    println(isEven(4))
    println(isOdd(7))
    println(isEven(3))
}
