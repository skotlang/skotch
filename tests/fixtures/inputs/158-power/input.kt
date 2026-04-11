fun power(base: Int, exp: Int): Int {
    var result = 1
    for (i in 1..exp) {
        result *= base
    }
    return result
}

fun main() {
    println(power(2, 10))
    println(power(3, 5))
    println(power(10, 3))
}
