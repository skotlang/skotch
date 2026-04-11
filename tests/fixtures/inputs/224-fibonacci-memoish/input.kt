fun main() {
    // Compute first 15 Fibonacci numbers iteratively
    var a = 0
    var b = 1
    for (i in 0..14) {
        println(a)
        val next = a + b
        a = b
        b = next
    }
}
