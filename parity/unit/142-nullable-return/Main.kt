fun safeDivide(a: Int, b: Int): Int? = if (b == 0) null else a / b

fun main() {
    println(safeDivide(10, 2))
    println(safeDivide(10, 0))
    val r = safeDivide(100, 5)
    println(r?.let { "ok=$it" } ?: "err")
    println(safeDivide(7, 0)?.let { "ok=$it" } ?: "err")
}
