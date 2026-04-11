fun fib(n: Int): Int {
    if (n <= 1) {
        return n
    }
    var prev = 0
    var curr = 1
    for (i in 2..n) {
        val next = prev + curr
        prev = curr
        curr = next
    }
    return curr
}

fun main() {
    for (i in 0..12) {
        println(fib(i))
    }
}
