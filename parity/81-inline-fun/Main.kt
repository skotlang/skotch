inline fun <T> measure(label: String, block: () -> T): T {
    val r = block()
    println("$label: $r")
    return r
}

fun main() {
    val sum = measure("sum") { 1 + 2 + 3 }
    val product = measure("prod") { 2 * 3 * 4 }
    println(sum + product)
}
