fun dayType(day: String): String = when (day) {
    "Saturday", "Sunday" -> "weekend"
    else -> "weekday"
}

fun main() {
    println(dayType("Monday"))
    println(dayType("Saturday"))
}
