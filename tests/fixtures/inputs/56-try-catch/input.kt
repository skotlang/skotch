fun main() {
    try {
        val x = 10 / 0
        println(x)
    } catch (e: ArithmeticException) {
        println("caught: division by zero")
    }
}
