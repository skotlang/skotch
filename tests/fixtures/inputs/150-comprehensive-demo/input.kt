fun Int.isEven(): Boolean = this % 2 == 0
fun Int.isOdd(): Boolean = !this.isEven()

fun sumOfOdds(limit: Int): Int {
    var total = 0
    for (i in 1..limit) {
        if (i.isEven()) {
            continue
        }
        total += i
    }
    return total
}

fun fibonacci(n: Int): Int {
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
    println("Sum of odds 1..10: ${sumOfOdds(10)}")
    println("Fib(10): ${fibonacci(10)}")

    val classification = when {
        sumOfOdds(10) > 20 -> "big"
        else -> "small"
    }
    println("Classification: $classification")

    for (i in 1..5) {
        println("$i: ${if (i.isOdd()) "odd" else "even"}")
    }
}
