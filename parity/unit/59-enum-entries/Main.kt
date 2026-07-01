enum class Color { RED, GREEN, BLUE }

fun main() {
    for (c in Color.entries) println(c)
    println(Color.entries.size)
}
