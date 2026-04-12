fun add(a: Int, b: Int = 10): Int = a + b

fun main() {
    println(add(5))
    println(add(5, 20))
}
