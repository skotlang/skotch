fun countMultiples(limit: Int, divisor: Int): Int {
    var count = 0
    for (i in 1..limit) {
        if (i % divisor == 0) {
            count += 1
        }
    }
    return count
}

fun main() {
    println(countMultiples(100, 3))
    println(countMultiples(100, 7))
    println(countMultiples(50, 5))
}
