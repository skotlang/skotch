fun stripPrefix(s: String, prefix: String): String =
    if (s.startsWith(prefix)) s.substring(prefix.length) else s

fun main() {
    println(stripPrefix("hello-world", "hello-"))
    println(stripPrefix("kotlin", "java"))
    println(stripPrefix("anything", ""))
    println(stripPrefix("abc", "abc"))
}
