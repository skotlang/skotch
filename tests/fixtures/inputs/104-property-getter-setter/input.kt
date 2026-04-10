class Temperature {
    var celsius: Double = 0.0
    val fahrenheit: Double
        get() = celsius * 9.0 / 5.0 + 32.0
}

fun main() {
    val t = Temperature()
    t.celsius = 100.0
    println(t.fahrenheit)
}
