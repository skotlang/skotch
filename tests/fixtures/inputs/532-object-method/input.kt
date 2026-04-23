object Utils {
    fun double(x: Int): Int = x * 2
    fun greet(name: String): String = "Hello, $name!"
}

fun main() {
    println(Utils.double(21))
    println(Utils.greet("World"))
}
