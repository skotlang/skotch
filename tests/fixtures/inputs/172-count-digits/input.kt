fun countDigits(n: Int): Int {
    if (n == 0) {
        return 1
    }
    var count = 0
    var x = n
    if (x < 0) {
        x = -x
    }
    while (x > 0) {
        x = x / 10
        count += 1
    }
    return count
}

fun main() {
    println(countDigits(0))
    println(countDigits(7))
    println(countDigits(42))
    println(countDigits(12345))
    println(countDigits(-99))
}
