fun reverse(n: Int): Int {
    var x = n
    var result = 0
    while (x > 0) {
        result = result * 10 + x % 10
        x = x / 10
    }
    return result
}

fun main() {
    println(reverse(12345))
    println(reverse(100))
    println(reverse(7))
}
