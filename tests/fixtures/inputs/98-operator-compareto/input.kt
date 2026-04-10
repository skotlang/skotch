class Temperature(val degrees: Double) : Comparable<Temperature> {
    override fun compareTo(other: Temperature): Int = degrees.compareTo(other.degrees)
    override fun toString(): String = "${degrees}°"
}

fun main() {
    val hot = Temperature(100.0)
    val cold = Temperature(0.0)
    println(hot > cold)
    println(cold < hot)
}
