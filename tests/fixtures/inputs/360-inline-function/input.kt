inline fun double(x: Int): Int = x * 2
inline fun greet(name: String): String = "Hello, " + name

fun main() {
    println(double(21))
    println(greet("World"))
}
