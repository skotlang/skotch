fun <T> identity(x: T): T = x

fun main() {
    println(identity(42))
    println(identity("hello"))
}
