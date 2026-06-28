fun grade(score: Int): String {
    if (score >= 90) return "A"
    else if (score >= 80) return "B"
    else if (score >= 70) return "C"
    else if (score >= 60) return "D"
    else return "F"
}

fun main() {
    for (s in listOf(95, 85, 75, 65, 50)) println(grade(s))
}
