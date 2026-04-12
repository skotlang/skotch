fun first(a: String?, b: String?): String = a ?: b ?: "none"

fun main() {
    println(first("hello", "world"))
    println(first(null, "world"))
    println(first(null, null))
}
