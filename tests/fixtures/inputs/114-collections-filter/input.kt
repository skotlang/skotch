fun main() {
    val numbers = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)
    val evens = numbers.filter { it % 2 == 0 }
    val odds = numbers.filter { it % 2 != 0 }
    println("evens: $evens")
    println("odds: $odds")
}
