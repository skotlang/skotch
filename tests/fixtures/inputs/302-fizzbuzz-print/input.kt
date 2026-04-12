fun main() {
    for (i in 1..20) {
        val result = when {
            i % 15 == 0 -> "FizzBuzz"
            i % 3 == 0 -> "Fizz"
            i % 5 == 0 -> "Buzz"
            else -> i.toString()
        }
        print(result)
        if (i < 20) print(", ")
    }
    println()
}
