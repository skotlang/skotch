fun describe(x: Int, y: Int): String {
    return when {
        x == 0 && y == 0 -> "origin"
        x == 0 -> "y-axis"
        y == 0 -> "x-axis"
        x > 0 && y > 0 -> "q1"
        x < 0 && y > 0 -> "q2"
        x < 0 && y < 0 -> "q3"
        else -> "q4"
    }
}

fun main() {
    for ((x, y) in listOf(0 to 0, 0 to 5, 5 to 0, 3 to 4, -3 to 4, -3 to -4, 3 to -4)) {
        println("($x,$y): ${describe(x, y)}")
    }
}
