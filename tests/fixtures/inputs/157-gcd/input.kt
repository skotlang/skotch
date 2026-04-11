fun gcd(a: Int, b: Int): Int {
    var x = a
    var y = b
    while (y != 0) {
        val temp = y
        y = x % y
        x = temp
    }
    return x
}

fun main() {
    println(gcd(12, 8))
    println(gcd(100, 75))
    println(gcd(17, 13))
}
