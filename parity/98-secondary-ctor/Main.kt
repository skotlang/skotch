class Temp(val celsius: Double) {
    constructor(f: Int) : this((f - 32) * 5.0 / 9.0)
    fun show(): String = "$celsius°C"
}

fun main() {
    println(Temp(100.0).show())
    println(Temp(32).show())
    println(Temp(212).show())
}
