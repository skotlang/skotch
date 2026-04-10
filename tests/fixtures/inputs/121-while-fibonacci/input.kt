fun fibonacci(n: Int): Int {
    var a = 0
    var b = 1
    var i = 0
    while (i < n) {
        val temp = a + b
        a = b
        b = temp
        i = i + 1
    }
    return a
}

fun main() {
    println(fibonacci(0))
    println(fibonacci(1))
    println(fibonacci(5))
    println(fibonacci(10))
}
