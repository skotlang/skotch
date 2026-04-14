fun main() {
    val outer = { x: Int ->
        val inner = { y: Int -> x + y }
        inner(10)
    }
    println(outer(5))
    println(outer(20))
}
