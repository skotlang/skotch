enum class Planet(val mass: Double, val radius: Double) {
    EARTH(5.976e24, 6.37814e6),
    MARS(6.421e23, 3.3972e6);

    fun surfaceGravity(): Double = 6.674e-11 * mass / (radius * radius)
}

fun main() {
    println(Planet.EARTH.surfaceGravity())
}
