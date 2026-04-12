fun max(a: Int, b: Int): Int = if (a > b) a else b
fun min(a: Int, b: Int): Int = if (a < b) a else b
fun clamp(value: Int, lo: Int, hi: Int): Int = max(lo, min(value, hi))

fun main() {
    println(max(10, 20))
    println(min(10, 20))
    println(clamp(50, 0, 100))
    println(clamp(-5, 0, 100))
    println(clamp(150, 0, 100))
}
