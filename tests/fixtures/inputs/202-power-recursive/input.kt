fun power(base: Int, exp: Int): Int {
    if (exp == 0) {
        return 1
    }
    if (exp % 2 == 0) {
        val half = power(base, exp / 2)
        return half * half
    }
    return base * power(base, exp - 1)
}

fun main() {
    println(power(2, 10))
    println(power(3, 5))
    println(power(5, 3))
}
