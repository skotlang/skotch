fun Int.factorial(): Int {
    if (this <= 1) return 1
    return this * (this - 1).factorial()
}

fun main() {
    println(5.factorial())
    println(0.factorial())
    println(10.factorial())
}
