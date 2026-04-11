fun add(a: Int, b: Int): Int = a + b
fun mul(a: Int, b: Int): Int = a * b

fun main() {
    println(add(mul(2, 3), mul(4, 5)))
    println(mul(add(1, 2), add(3, 4)))
}
