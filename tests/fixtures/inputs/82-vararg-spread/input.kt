fun printAll(vararg items: String) {
    for (item in items) println(item)
}

fun main() {
    val words = arrayOf("hello", "world")
    printAll(*words)
}
