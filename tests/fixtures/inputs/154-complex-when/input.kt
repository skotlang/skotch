fun fizzBuzz(n: Int): String = when {
    n % 15 == 0 -> "FizzBuzz"
    n % 3 == 0 -> "Fizz"
    n % 5 == 0 -> "Buzz"
    else -> "other"
}

fun main() {
    println(fizzBuzz(3))
    println(fizzBuzz(5))
    println(fizzBuzz(15))
    println(fizzBuzz(7))
}
