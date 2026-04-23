data class Student(val name: String, val grade: Int)

fun classify(grade: Int): String = when {
    grade >= 90 -> "A"
    grade >= 80 -> "B"
    grade >= 70 -> "C"
    else -> "F"
}

fun main() {
    val students = listOf(
        Student("Alice", 95),
        Student("Bob", 82),
        Student("Carol", 67)
    )
    for (s in students) {
        println("${s.name}: ${classify(s.grade)}")
    }
}
