fun printCategory(n: Int) {
    when {
        n < 0 -> println("negative")
        n == 0 -> println("zero")
        n < 10 -> println("small")
        else -> println("large")
    }
}

fun main() {
    printCategory(-5)
    printCategory(0)
    printCategory(7)
    printCategory(100)
}
