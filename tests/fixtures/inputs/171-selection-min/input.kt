fun min3(a: Int, b: Int, c: Int): Int {
    if (a <= b && a <= c) {
        return a
    }
    if (b <= c) {
        return b
    }
    return c
}

fun main() {
    println(min3(3, 1, 2))
    println(min3(5, 9, 7))
    println(min3(10, 10, 10))
}
