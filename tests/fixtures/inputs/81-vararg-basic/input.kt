fun sum(vararg numbers: Int): Int {
    var total = 0
    for (n in numbers) total += n
    return total
}

fun main() {
    println(sum(1, 2, 3, 4, 5))
}
