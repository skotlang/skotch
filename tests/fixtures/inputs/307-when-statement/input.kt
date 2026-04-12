fun main() {
    val x = 3
    when (x) {
        1 -> println("one")
        2 -> println("two")
        3 -> println("three")
        else -> println("other")
    }
    when {
        x < 0 -> println("negative")
        x == 0 -> println("zero")
        else -> println("positive")
    }
}
