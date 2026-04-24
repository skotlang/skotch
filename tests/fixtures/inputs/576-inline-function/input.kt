inline fun <reified T> isType(value: Any): Boolean = value is T

inline fun double(x: Int): Int = x * 2

fun main() {
    println(isType<String>("hello"))
    println(isType<Int>("hello"))
    println(double(21))
}
