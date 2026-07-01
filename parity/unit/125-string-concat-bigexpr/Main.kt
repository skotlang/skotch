fun main() {
    val a = 7
    val b = 11
    val c = 13
    val msg = "computed " + a + "+" + b + "+" + c + " = " + (a + b + c)
    println(msg)
    val parts = "[" + listOf(a, b, c).joinToString(",") + "]"
    println(parts)
}
