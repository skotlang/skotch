enum class Color(val hex: Int) {
    RED(0xFF0000),
    GREEN(0x00FF00),
    BLUE(0x0000FF)
}

fun main() {
    println(Color.RED)
    println(Color.GREEN.hex)
    println(Color.BLUE.hex)
}
