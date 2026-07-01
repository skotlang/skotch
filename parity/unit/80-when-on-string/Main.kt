fun classify(s: String): String = when (s) {
    "yes", "y" -> "affirmative"
    "no", "n" -> "negative"
    "" -> "empty"
    else -> "unknown:$s"
}

fun main() {
    println(classify("yes"))
    println(classify("n"))
    println(classify(""))
    println(classify("maybe"))
}
