fun main() {
    var i = 0
    do {
        println("at $i")
        i++
    } while (i < 3)

    var n = 10
    do {
        n -= 3
    } while (n > 0)
    println("final: $n")
}
