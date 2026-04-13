enum class Color(val hex: Int) {
    RED(16711680),
    GREEN(65280),
    BLUE(255)
}

fun main() {
    println(Color.RED)
    println(Color.RED.hex)
    println(Color.GREEN.hex)
    println(Color.BLUE.hex)
}
