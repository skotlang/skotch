fun isDigits(s: String): Boolean = s.matches("^[0-9]+$")

fun main() {
    println(isDigits("12345"))
    println(isDigits("hello"))
}
