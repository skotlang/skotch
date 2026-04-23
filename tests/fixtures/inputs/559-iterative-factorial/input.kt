fun factorial(n: Int): Int {
    var result = 1
    for (i in 2..n) {
        result *= i
    }
    return result
}
fun main() {
    println(factorial(5))
    println(factorial(10))
}
