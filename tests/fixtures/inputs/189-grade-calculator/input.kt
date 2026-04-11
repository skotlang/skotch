fun grade(score: Int): String = when {
    score >= 90 -> "A"
    score >= 80 -> "B"
    score >= 70 -> "C"
    score >= 60 -> "D"
    else -> "F"
}

fun main() {
    println(grade(95))
    println(grade(85))
    println(grade(72))
    println(grade(61))
    println(grade(45))
}
