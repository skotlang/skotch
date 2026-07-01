fun main() {
    val isEven: Predicate = { n -> n % 2 == 0 }
    val gtTen: Predicate = { n -> n > 10 }

    val even = CountingMatcher(isEven)
    val big = CountingMatcher(gtTen)

    val samples = listOf(2, 5, 11, 14, 7, 20)
    var i = 0
    while (i < samples.size) {
        val x = samples[i]
        val m1 = even.matches(x)
        val m2 = big.matches(x)
        println("$x even=$m1 big=$m2")
        i += 1
    }

    println("even hits: ${even.hits()}")
    println("big  hits: ${big.hits()}")

    even.reset()
    println("after reset, even hits: ${even.hits()}")
}
