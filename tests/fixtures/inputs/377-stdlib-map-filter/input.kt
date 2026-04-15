fun main() {
    val nums = listOf(1, 2, 3, 4, 5)
    val doubled = nums.map { it * 2 }
    println(doubled)
    val evens = nums.filter { it % 2 == 0 }
    println(evens)
}
