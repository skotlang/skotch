class Multiplier(val factor: Int) {
    fun multiply(n: Int): Int = n * factor
}

fun main() {
    val m = Multiplier(3)
    println(m.multiply(7))
    println(m.multiply(10))
}
