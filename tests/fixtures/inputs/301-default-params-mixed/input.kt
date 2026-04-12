fun format(value: Int, prefix: String = "[", suffix: String = "]"): String {
    return "$prefix$value$suffix"
}

fun main() {
    println(format(42))
    println(format(42, "("))
    println(format(42, "(", ")"))
}
