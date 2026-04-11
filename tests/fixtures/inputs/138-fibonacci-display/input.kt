fun fib(n: Int): Int {
    if (n <= 1) {
        return n
    }
    var a = 0
    var b = 1
    for (i in 2..n) {
        val temp = a + b
        a = b
        b = temp
    }
    return b
}

fun main() {
    for (i in 0..10) {
        println("fib($i) = ${fib(i)}")
    }
}
