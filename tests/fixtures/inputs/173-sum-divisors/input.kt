fun sumDivisors(n: Int): Int {
    var sum = 0
    for (i in 1..n) {
        if (n % i == 0) {
            sum += i
        }
    }
    return sum
}

fun main() {
    println(sumDivisors(6))
    println(sumDivisors(12))
    println(sumDivisors(28))
}
