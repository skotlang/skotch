fun power(base: Int, exp: Int): Int {
    if (exp == 0) return 1
    return base * power(base, exp - 1)
}

fun main() {
    println(power(2, 10))
    println(power(3, 4))
    println(power(5, 0))
}
