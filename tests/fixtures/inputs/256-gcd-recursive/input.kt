fun gcd(a: Int, b: Int): Int {
    if (b == 0) return a
    return gcd(b, a % b)
}

fun main() {
    println(gcd(48, 18))
    println(gcd(100, 75))
}
