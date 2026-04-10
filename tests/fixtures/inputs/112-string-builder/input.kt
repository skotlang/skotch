fun main() {
    val sb = StringBuilder()
    for (i in 1..5) {
        sb.append(i)
        if (i < 5) sb.append(", ")
    }
    println(sb.toString())
}
