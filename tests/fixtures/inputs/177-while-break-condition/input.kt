fun main() {
    var i = 0
    while (i < 100) {
        i += 1
        if (i * i > 50) {
            break
        }
    }
    println(i)
}
