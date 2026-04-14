fun main() {
    val adder = { x: Int ->
        val f = { y: Int -> x + y }
        f
    }
    val add5 = adder(5)
    println(add5(3))
    println(add5(10))
}
