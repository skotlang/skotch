fun printMonth(m: Int) {
    when (m) {
        1 -> println("January")
        2 -> println("February")
        3 -> println("March")
        4 -> println("April")
        5 -> println("May")
        6 -> println("June")
        else -> println("Other")
    }
}

fun main() {
    printMonth(1)
    printMonth(3)
    printMonth(6)
    printMonth(12)
}
