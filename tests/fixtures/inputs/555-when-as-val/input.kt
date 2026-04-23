fun main() {
    for (i in 1..5) {
        val desc = when {
            i % 2 == 0 -> "even"
            else -> "odd"
        }
        println("$i is $desc")
    }
}
