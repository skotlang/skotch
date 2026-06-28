fun isPrime(n: Int): Boolean {
    if (n < 2) return false
    if (n < 4) return true
    if (n % 2 == 0) return false
    var i = 3
    while (i * i <= n) {
        if (n % i == 0) return false
        i += 2
    }
    return true
}

fun main() {
    var count = 0
    for (n in 2..30) {
        if (isPrime(n)) {
            print("$n ")
            count++
        }
    }
    println()
    println("count=$count")
}
