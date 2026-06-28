fun main() {
    val words = listOf("hello", "world", "kotlin")
    val chars = words.flatMap { it.toList() }
    println(chars.size)
    println(chars.distinct().sorted())
    val nums = listOf(listOf(1, 2), listOf(3, 4, 5), listOf(6))
    val flat = nums.flatten()
    println(flat)
    println(flat.sum())
}
