fun dayName(day: Int): String = when (day) {
    1 -> "Monday"
    2 -> "Tuesday"
    3 -> "Wednesday"
    4 -> "Thursday"
    5 -> "Friday"
    6, 7 -> "Weekend"
    else -> "Unknown"
}

fun main() {
    for (d in 1..8) {
        println(dayName(d))
    }
}
