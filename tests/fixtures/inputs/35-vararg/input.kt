// TODO: vararg parameters lower to a `T[]` array.
fun sum(vararg xs: Int): Int {
    var total = 0
    for (x in xs) total += x
    return total
}
