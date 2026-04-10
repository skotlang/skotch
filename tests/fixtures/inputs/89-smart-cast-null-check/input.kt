fun printLength(s: String?) {
    if (s != null) {
        println(s.length)
    } else {
        println("null")
    }
}

fun main() {
    printLength("hello")
    printLength(null)
}
