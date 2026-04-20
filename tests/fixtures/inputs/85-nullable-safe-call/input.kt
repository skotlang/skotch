fun len(s: String?): String = s?.uppercase() ?: "null"

fun main() {
    println(len("hello"))
    println(len(null))
}
