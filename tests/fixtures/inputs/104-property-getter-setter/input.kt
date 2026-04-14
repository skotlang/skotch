class Temperature(var celsius: Double) {
    val fahrenheit: Double
        get() = celsius * 9.0 / 5.0 + 32.0
}

fun main() {
    val t = Temperature(100.0)
    println(t.fahrenheit)
}
