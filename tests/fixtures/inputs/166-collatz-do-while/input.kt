fun main() {
    var n = 100
    var steps = 0
    do {
        if (n % 2 == 0) {
            n = n / 2
        } else {
            n = n * 3 + 1
        }
        steps += 1
    } while (n != 1)
    println(steps)
}
