fun firstChar(s: String): Char = s[0]
fun nth(s: String, i: Int): Char = s[i]

fun main() {
    println(firstChar("hello"))
    println(nth("kotlin", 3))
    val s = "abc"
    for (i in 0 until s.length) println(s[i])
}
