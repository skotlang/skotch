fun buildString(block: StringBuilder.() -> Unit): String {
    val sb = StringBuilder()
    sb.block()
    return sb.toString()
}

fun main() {
    val result = buildString {
        append("Hello")
        append(", ")
        append("world!")
    }
    println(result)
}
