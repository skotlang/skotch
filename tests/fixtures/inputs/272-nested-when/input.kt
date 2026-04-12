fun dayType(day: Int): String = when (day) {
    1, 7 -> "weekend"
    2, 3, 4, 5, 6 -> "weekday"
    else -> "invalid"
}

fun main() {
    println(dayType(1))
    println(dayType(3))
    println(dayType(7))
    println(dayType(0))
}
