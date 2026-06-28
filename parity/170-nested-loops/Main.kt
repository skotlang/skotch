fun main() {
    for (i in 1..3) {
        for (j in 1..3) {
            print("${i * j} ")
        }
        println()
    }
    var total = 0
    for (i in 1..5) {
        for (j in 1..5) {
            if (j > i) break
            total += i * j
        }
    }
    println("total=$total")
}
