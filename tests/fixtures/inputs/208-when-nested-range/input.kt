fun category(score: Int): String = when (score) {
    in 90..100 -> "A"
    in 80..89 -> "B"
    in 70..79 -> "C"
    in 60..69 -> "D"
    in 0..59 -> "F"
    else -> "Invalid"
}

fun main() {
    println(category(95))
    println(category(85))
    println(category(72))
    println(category(65))
    println(category(45))
    println(category(-1))
}
