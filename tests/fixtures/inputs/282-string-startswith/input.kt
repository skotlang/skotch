fun isUrl(s: String): Boolean = s.startsWith("http")

fun main() {
    println(isUrl("https://example.com"))
    println(isUrl("ftp://files.example.com"))
    println(isUrl("not a url"))
}
