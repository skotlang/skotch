fun describe(s: String?): String {
    if (s == null) return "null"
    return "value(${s.length})=$s"
}

fun maybeLen(s: String?): Int = s?.length ?: 0

fun main() {
    println(describe("hello"))
    println(describe(null))
    println(maybeLen("kotlin"))
    println(maybeLen(null))
    val s: String? = "test"
    println(s!!.length)
}
