fun abs(n: Int): Int = if (n < 0) -n else n
fun min(a: Int, b: Int): Int = if (a < b) a else b
fun max(a: Int, b: Int): Int = if (a > b) a else b

fun gcd(a: Int, b: Int): Int {
    var x = abs(a)
    var y = abs(b)
    while (y != 0) {
        val t = y
        y = x % y
        x = t
    }
    return x
}

fun lcm(a: Int, b: Int): Int = a / gcd(a, b) * b

fun main() {
    println(abs(-42))
    println(min(3, 7))
    println(max(3, 7))
    println(gcd(12, 8))
    println(lcm(4, 6))
}
