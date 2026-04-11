fun min(a: Int, b: Int): Int = if (a < b) a else b
fun max(a: Int, b: Int): Int = if (a > b) a else b
fun clamp(value: Int, lo: Int, hi: Int): Int = max(lo, min(value, hi))

fun main() {
    println(min(3, 7))
    println(max(3, 7))
    println(clamp(5, 1, 10))
    println(clamp(-5, 0, 100))
    println(clamp(200, 0, 100))
}
