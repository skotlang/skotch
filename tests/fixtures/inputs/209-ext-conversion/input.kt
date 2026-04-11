fun Int.toFahrenheit(): Int = this * 9 / 5 + 32
fun Int.toCelsius(): Int = (this - 32) * 5 / 9

fun main() {
    println(0.toFahrenheit())
    println(100.toFahrenheit())
    println(32.toCelsius())
    println(212.toCelsius())
}
