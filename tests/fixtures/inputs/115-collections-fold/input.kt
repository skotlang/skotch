fun main() {
    val sum = listOf(1, 2, 3, 4, 5).fold(0) { acc, n -> acc + n }
    println(sum)
}
