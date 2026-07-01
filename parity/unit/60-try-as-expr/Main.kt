fun parseOrZero(s: String): Int {
    val r = try { s.toInt() } catch (e: NumberFormatException) { 0 }
    return r
}

fun main() {
    println(parseOrZero("42"))
    println(parseOrZero("nope"))
}
