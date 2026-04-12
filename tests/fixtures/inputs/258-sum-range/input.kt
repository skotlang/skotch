fun sumRange(start: Int, end: Int): Int {
    var sum = 0
    for (i in start..end) {
        sum = sum + i
    }
    return sum
}

fun main() {
    println(sumRange(1, 100))
    println(sumRange(1, 10))
}
