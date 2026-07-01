fun sum(vararg xs: Int): Int {
    var t = 0
    for (x in xs) t += x
    return t
}

fun main() {
    val arr = intArrayOf(1, 2, 3, 4)
    println(sum(*arr))
}
