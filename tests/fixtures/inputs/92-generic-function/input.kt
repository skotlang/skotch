fun <T> identity(x: T): T = x

fun <T> pair(a: T, b: T): String = "$a and $b"

fun main() {
    println(identity(42))
    println(identity("hello"))
    println(pair(1, 2))
}
