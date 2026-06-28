fun findLen(s: String?): Int = s?.length ?: 0

fun firstNonEmpty(vararg xs: String): String {
    for (x in xs) if (x.isNotEmpty()) return x
    return ""
}

fun main() {
    println(findLen("hello"))
    println(findLen(""))
    println(findLen(null))
    println(firstNonEmpty("", "", "yo", "later"))
    println(firstNonEmpty("", ""))
}
