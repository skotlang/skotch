fun add(a: Int, b: Int): Int = a + b
fun mul(a: Int, b: Int): Int = a * b
fun sub(a: Int, b: Int): Int = a - b

fun main() {
    println(add(mul(2, 3), sub(10, 4)))
    println(mul(add(1, 2), add(3, 4)))
    println(sub(mul(5, 5), add(1, 1)))
    println(add(add(1, 2), add(3, add(4, 5))))
}
