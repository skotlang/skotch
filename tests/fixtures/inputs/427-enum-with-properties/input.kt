enum class Planet(val mass: Double) {
    EARTH(5.97),
    MARS(0.64),
    JUPITER(1898.0)
}

fun main() {
    println(Planet.EARTH.mass)
    println(Planet.MARS.name)
    println(Planet.JUPITER.mass)
}
