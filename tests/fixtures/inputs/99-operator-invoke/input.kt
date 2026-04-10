class Multiplier(val factor: Int) {
    operator fun invoke(x: Int): Int = x * factor
}

fun main() {
    val double = Multiplier(2)
    println(double(5))
    println(double(21))
}
