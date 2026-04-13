fun notString(x: Any): Boolean = x !is String

fun main() {
    println(notString("hello"))
    println(notString(42))
}
