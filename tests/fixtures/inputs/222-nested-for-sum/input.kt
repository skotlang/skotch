fun main() {
    var total = 0
    for (i in 1..5) {
        for (j in 1..5) {
            total += i * j
        }
    }
    println(total)
}
