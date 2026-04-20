fun sum(vararg xs: Int): Int {
    var total = 0
    for (x in xs) total += x
    return total
}

fun main() {
    println(sum(1, 2, 3))
}
