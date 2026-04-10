fun firstChar(s: String?): Char? = s?.get(0)

fun main() {
    println(firstChar("hello"))
    println(firstChar(null))
}
