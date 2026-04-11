val MAX = 100
val GREETING = "Hello"

fun clamp(n: Int): Int {
    if (n > MAX) {
        return MAX
    }
    if (n < 0) {
        return 0
    }
    return n
}

fun main() {
    println(GREETING)
    println(clamp(50))
    println(clamp(200))
    println(clamp(-5))
}
