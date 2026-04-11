fun outer(x: Int): Int {
    fun inner(y: Int): Int = y * 2
    return inner(x) + 1
}

fun main() {
    println(outer(5))
    println(outer(10))
}
