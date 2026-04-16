enum class Color { RED, GREEN, BLUE }
fun main() {
    val colors = Color.values()
    for (c in colors) {
        println(c)
    }
    println(Color.valueOf("GREEN"))
}
