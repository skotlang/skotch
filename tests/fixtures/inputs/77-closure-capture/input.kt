fun makeCounter(): () -> Int {
    var count = 0
    return { count++ }
}

fun main() {
    val counter = makeCounter()
    println(counter())
    println(counter())
    println(counter())
}
