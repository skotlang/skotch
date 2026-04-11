fun sumTo(n: Int): Int {
    var total = 0
    for (i in 1..n) {
        total = total + i
    }
    return total
}

fun main() {
    println(sumTo(10))
    println(sumTo(100))
}
