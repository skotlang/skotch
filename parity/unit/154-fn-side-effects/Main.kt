var globalCount = 0

fun bump(): Int {
    globalCount++
    return globalCount
}

fun main() {
    println(bump())
    println(bump())
    println(bump())
    println("total=$globalCount")
    val results = listOf(bump(), bump(), bump())
    println(results)
    println("final=$globalCount")
}
