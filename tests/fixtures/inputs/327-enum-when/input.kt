enum class Season { SPRING, SUMMER, AUTUMN, WINTER }

fun describe(s: String): String = when (s) {
    "SPRING" -> "flowers bloom"
    "SUMMER" -> "sun shines"
    "AUTUMN" -> "leaves fall"
    "WINTER" -> "snow falls"
    else -> "unknown"
}

fun main() {
    println(describe(Season.SPRING))
    println(describe(Season.WINTER))
}
