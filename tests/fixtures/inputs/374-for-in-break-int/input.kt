fun main() {
    val items = listOf(1, 2, 3, 4, 5)
    for (item in items) {
        if (item == 3) break
        println(item)
    }
}
