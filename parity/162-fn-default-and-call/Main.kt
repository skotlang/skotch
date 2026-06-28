fun add(a: Int, b: Int = 10, c: Int = 100): Int = a + b + c

fun main() {
    println(add(1))
    println(add(1, 2))
    println(add(1, 2, 3))
    println(add(1, c = 50))
    println(add(7, b = 3))
}
