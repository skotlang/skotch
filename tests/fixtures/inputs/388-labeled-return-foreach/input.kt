fun main() {
    listOf(1, 2, 3, 4, 5).forEach {
        if (it == 3) return@forEach
        println(it)
    }
    println("done")
}
