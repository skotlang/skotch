enum class Color(val hex: String) {
    RED("#FF0000"),
    GREEN("#00FF00"),
    BLUE("#0000FF")
}

fun main() {
    println(Color.RED.name)
    println(Color.RED.hex)
    println(Color.BLUE.hex)
}
