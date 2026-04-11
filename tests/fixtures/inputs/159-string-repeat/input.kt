fun repeat(s: String, n: Int): String {
    var result = ""
    for (i in 1..n) {
        result = result + s
    }
    return result
}

fun main() {
    println(repeat("ab", 3))
    println(repeat("x", 5))
}
