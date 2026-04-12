fun main() {
    var sum1 = 0
    for (i in 1..10) { sum1 += i }
    println(sum1)
    var sum2 = 0
    for (i in 1 until 11) { sum2 += i }
    println(sum2)
}
