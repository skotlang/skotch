for (n in 2..20) {
    var isPrime = true
    var i = 2
    while (i * i <= n) {
        if (n % i == 0) {
            isPrime = false
        }
        i = i + 1
    }
    if (isPrime) {
        println(n)
    }
}
