fun Int.digitSum(): Int {
    var n = if (this < 0) -this else this
    var sum = 0
    while (n > 0) {
        sum += n % 10
        n /= 10
    }
    return sum
}

fun main() {
    println(123.digitSum())
    println(9999.digitSum())
    println(0.digitSum())
}
