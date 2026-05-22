fun makeStr(a: String, b: Int, c: Boolean): String {
    return "$a/$b/$c"
}

fun consumeFunction3(f: (String, Int, Boolean) -> String): String {
    return f("hi", 7, true)
}

fun main() {
    val result = consumeFunction3(::makeStr)
    println(result)
}
