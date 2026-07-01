fun main() {
    val a = listOf("a", "b", "c")
    val b = listOf(1, 2, 3, 4)
    val zipped = a.zip(b)
    for ((s, n) in zipped) println("$s=$n")
    println(zipped.size)
    val sums = listOf(1, 2, 3).zip(listOf(10, 20, 30)) { x, y -> x + y }
    println(sums)
}
