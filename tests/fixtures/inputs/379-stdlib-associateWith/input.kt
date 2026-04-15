fun main() {
    val words = listOf("apple", "banana", "cherry")
    val map = words.associateWith { it.length }
    println(map)
}
