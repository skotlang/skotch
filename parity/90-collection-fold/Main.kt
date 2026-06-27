fun main() {
    val xs = listOf(1, 2, 3, 4, 5)
    val sum = xs.fold(0) { acc, x -> acc + x }
    val prod = xs.fold(1) { acc, x -> acc * x }
    val joined = xs.fold("") { acc, x -> if (acc.isEmpty()) "$x" else "$acc,$x" }
    println(sum)
    println(prod)
    println(joined)
}
