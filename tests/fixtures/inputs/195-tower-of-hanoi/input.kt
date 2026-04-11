fun hanoi(n: Int): Int {
    if (n <= 0) {
        return 0
    }
    return hanoi(n - 1) + 1 + hanoi(n - 1)
}

fun main() {
    println(hanoi(1))
    println(hanoi(3))
    println(hanoi(4))
}
