fun isPrime(n: Int): Boolean {
    if (n < 2) return false
    var i = 2
    while (i * i <= n) {
        if (n % i == 0) return false
        i = i + 1
    }
    return true
}

fun main() {
    for (n in 2..30) {
        if (isPrime(n)) {
            println(n)
        }
    }
}
