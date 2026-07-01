fun main() {
    for (i in 1..10) {
        if (i == 7) break
        if (i % 2 == 0) continue
        println(i)
    }

    var n = 0
    while (n < 100) {
        n++
        if (n == 3) continue
        if (n == 5) break
        println("n=$n")
    }
}
