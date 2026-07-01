fun main() {
    val nums = listOf(1, 2, 3, 4, 5, 6, 7, 8, 9, 10)

    val evens = nums.filter { it % 2 == 0 }
    println("evens = $evens")

    val squares = nums.map { it * it }
    println("squares = $squares")

    val sum = nums.fold(0) { acc, n -> acc + n }
    println("sum = $sum")

    val pairs = nums.zip(squares)
    println("zip first 3 = ${pairs.take(3)}")

    val byParity = nums.groupBy { if (it % 2 == 0) "even" else "odd" }
    println("evens grouped = ${byParity["even"]}")
    println("odds  grouped = ${byParity["odd"]}")

    val joined = nums.filter { it > 5 }.joinToString(prefix = "[", postfix = "]", separator = ", ")
    println("> 5 = $joined")

    val total = nums.sum()
    val max   = nums.max()
    val avg   = nums.average()
    println("total=$total max=$max avg=$avg")
}
