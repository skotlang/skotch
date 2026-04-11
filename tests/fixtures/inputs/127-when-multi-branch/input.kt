fun grade(score: Int): String = when (score) {
    10, 9 -> "A"
    8 -> "B"
    7 -> "C"
    else -> "F"
}

fun main() {
    println(grade(10))
    println(grade(9))
    println(grade(8))
    println(grade(7))
    println(grade(3))
}
