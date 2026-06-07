class C(val x: Double) {
    fun ratio(): Double {
        return halve(x)
    }
}

fun halve(d: Double): Double = d / 2.0

fun main() {
    println(C(10.0).ratio())
}
