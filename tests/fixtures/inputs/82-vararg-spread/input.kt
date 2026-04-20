fun sum(vararg xs: Int): Int {
    var total = 0
    for (x in xs) total += x
    return total
}

fun main() {
    val nums = intArrayOf(10, 20, 30)
    println(sum(*nums))
}
