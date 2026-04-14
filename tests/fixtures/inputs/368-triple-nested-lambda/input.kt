fun main() {
    val f = { a: Int ->
        val g = { b: Int ->
            val h = { c: Int -> a + b + c }
            h(3)
        }
        g(2)
    }
    println(f(1))
}
