fun Int.isPrime(): Boolean {
    if (this < 2) {
        return false
    }
    var i = 2
    while (i * i <= this) {
        if (this % i == 0) {
            return false
        }
        i += 1
    }
    return true
}

fun Int.factorial(): Int {
    var result = 1
    for (i in 2..this) {
        result *= i
    }
    return result
}

fun main() {
    // Primes up to 20
    for (n in 2..20) {
        if (n.isPrime()) {
            println(n)
        }
    }
    println()
    // Factorials
    for (n in 1..7) {
        println(n.factorial())
    }
}
