fun abs(x: Int): Int {
    if (x < 0) {
        return -x
    }
    return x
}

fun max(a: Int, b: Int): Int {
    if (a > b) {
        return a
    }
    return b
}

fun main() {
    println(abs(-42))
    println(abs(7))
    println(max(10, 20))
    println(max(99, 1))
}
