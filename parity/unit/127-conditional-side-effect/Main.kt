fun main() {
    var counter = 0
    val xs = listOf(1, 2, 3, 4, 5, 6, 7)
    for (x in xs) {
        if (x % 2 == 0) {
            counter++
        }
    }
    println("evens: $counter")

    var sum = 0
    for (x in xs) if (x > 3) sum += x
    println("sum>3: $sum")
}
