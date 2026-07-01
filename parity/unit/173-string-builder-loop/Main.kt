fun csv(xs: List<Int>): String {
    val sb = StringBuilder()
    for (i in xs.indices) {
        if (i > 0) sb.append(", ")
        sb.append(xs[i])
    }
    return sb.toString()
}

fun main() {
    println(csv(listOf(1, 2, 3, 4, 5)))
    println(csv(listOf(99)))
    println(csv(listOf()))
}
