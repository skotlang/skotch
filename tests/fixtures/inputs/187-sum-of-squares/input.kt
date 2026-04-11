fun Int.squared(): Int = this * this

fun sumOfSquares(n: Int): Int {
    var total = 0
    for (i in 1..n) {
        total += i.squared()
    }
    return total
}

fun main() {
    println(sumOfSquares(3))
    println(sumOfSquares(5))
    println(sumOfSquares(10))
}
