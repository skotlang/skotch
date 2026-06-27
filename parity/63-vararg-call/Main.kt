fun sum(vararg xs: Int): Int {
    var t = 0
    for (x in xs) t += x
    return t
}

fun main() {
    println(sum())
    println(sum(1, 2, 3))
    println(sum(7, 8))
}
